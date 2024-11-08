use std::{array, borrow::Cow, collections::HashMap, fmt::Display, hash::BuildHasherDefault, ops};

use ahash::AHasher;
use gc_arena::{
    arena::CollectionPhase,
    lock::{GcRefLock, RefLock},
    Arena, Collect, Collection, Gc, Mutation, Rootable,
};

use crate::{
    ai, builtins,
    fuel::Fuel,
    object::{BoundMethod, Class, Closure, Function, Instance, NativeFn, Upvalue, UpvalueObj},
    string::{InternedString, InternedStringSet},
    OpCode, ReturnValue, Value,
};

const FRAME_MAX_SIZE: usize = 64;
// const STACK_MAX_SIZE: usize = FRAME_MAX_SIZE * (u8::MAX as usize + 1);
#[cfg(not(test))]
const STACK_MAX_SIZE: usize = 4096; // Temporary reduce the stack size due to tokio thread stack size limit
#[cfg(test)]
const STACK_MAX_SIZE: usize = 128;

static NUMBER_OPERATOR_ERROR: &str = "Operands must be numbers.";

macro_rules! binary_op {
    ($self:expr, $op:tt) => {
        let b = $self.pop_stack().as_number().map_err(|_| $self.runtime_error(NUMBER_OPERATOR_ERROR.into()))?;
        let a = $self.pop_stack().as_number().map_err(|_| $self.runtime_error(NUMBER_OPERATOR_ERROR.into()))?;
        $self.push_stack((a $op b).into());
    };
}

pub type Table<'gc> = HashMap<InternedString<'gc>, Value<'gc>, BuildHasherDefault<AHasher>>;

#[derive(Debug)]
pub enum VmError {
    CompileError,
    RuntimeError(std::string::String),
}

impl std::error::Error for VmError {}

impl Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompileError => write!(f, "CompileError"),
            Self::RuntimeError(s) => write!(f, "RuntimeError: {s}"),
        }
    }
}

pub struct State<'gc> {
    mc: &'gc Mutation<'gc>,
    chunks: HashMap<usize, Gc<'gc, Function<'gc>>>,
    frames: Vec<CallFrame<'gc>>,
    frame_count: usize,
    stack: [Value<'gc>; STACK_MAX_SIZE],
    stack_top: usize,
    strings: InternedStringSet<'gc>,
    globals: Table<'gc>,
    open_upvalues: Option<GcRefLock<'gc, UpvalueObj<'gc>>>,
}

unsafe impl<'gc> Collect for State<'gc> {
    fn needs_trace() -> bool
    where
        Self: Sized,
    {
        true
    }

    fn trace(&self, cc: &Collection) {
        self.frames.trace(cc);
        self.frame_count.trace(cc);
        self.stack.trace(cc);
        self.stack_top.trace(cc);
        self.strings.trace(cc);
        self.globals.trace(cc);
        self.open_upvalues.trace(cc);
    }
}

pub struct Vm {
    arena: Arena<Rootable![State<'_>]>,
}

#[derive(Debug, Clone, Collect)]
#[collect(no_drop)]
struct CallFrame<'gc> {
    closure: Gc<'gc, Closure<'gc>>,
    // When we return from a function, the VM will
    // jump to the ip of the caller’s CallFrame and resume from there.
    ip: usize,
    // slot_start field points into the VM’s value stack
    // at the first slot that this function can use
    slot_start: usize,
}

impl<'gc> CallFrame<'gc> {
    fn next_opcode(&mut self) -> OpCode {
        let byte = self.closure.function[self.ip];
        self.ip += 1;
        byte
    }

    fn read_constant(&mut self, byte: u8) -> Value<'gc> {
        self.closure.function.read_constant(byte)
    }

    #[allow(unused)]
    fn disassemble(&self) {
        self.closure
            .function
            .disassemble(self.closure.function.name.unwrap().display_lossy());
    }

    #[allow(unused)]
    fn disassemble_instruction(&self, offset: usize) {
        self.closure.function.disassemble_instruction(offset);
    }
}

impl<'gc> State<'gc> {
    fn new(mc: &'gc Mutation<'gc>) -> Self {
        State {
            mc,
            chunks: HashMap::new(),
            frames: Vec::with_capacity(FRAME_MAX_SIZE),
            frame_count: 0,
            stack: array::from_fn(|_| Value::Nil),
            stack_top: 0,
            strings: InternedStringSet::new(mc),
            globals: HashMap::default(),
            open_upvalues: None,
        }
    }

    pub fn intern(&mut self, s: &[u8]) -> InternedString<'gc> {
        self.strings.intern(self.mc, s)
    }

    pub fn intern_static(&mut self, s: &'static str) -> InternedString<'gc> {
        self.strings.intern_static(self.mc, s.as_bytes())
    }

    pub fn get_chunk(&mut self, chunk_id: usize) -> Result<Gc<'gc, Function<'gc>>, VmError> {
        Ok(self.chunks.get(&chunk_id).copied().unwrap())
    }

    pub fn call_function(&mut self, chunk_id: usize) -> Result<(), VmError> {
        let function = self.get_chunk(chunk_id)?;
        #[cfg(feature = "debug")]
        function.disassemble("script");

        let closure = Gc::new(self.mc, Closure::new(self.mc, function));
        self.push_stack(Value::from(closure));
        self.call(closure, function.arity)
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    pub fn new() -> Self {
        Vm {
            arena: Arena::<Rootable![State<'_>]>::new(|mc| State::new(mc)),
        }
    }

    #[cfg(feature = "v1")]
    pub fn compile(&mut self, source: &'static str) -> Result<(), VmError> {
        self.arena.mutate_root(|mc, state| {
            let context = Context {
                mutation: mc,
                strings: state.strings,
            };
            let function = crate::v1::compile(context, source)?;
            #[cfg(feature = "debug")]
            function.disassemble("script");
            state.define_builtins();
            let closure = Gc::new(mc, Closure::new(mc, Gc::new(mc, function)));
            state.push_stack(Value::from(closure));
            state.call(closure, 0)
        })?;
        Ok(())
    }

    #[cfg(not(feature = "v1"))]
    pub fn compile(&mut self, source: &'static str) -> Result<(), VmError> {
        self.arena.mutate_root(|mc, state| {
            let context = Context {
                mutation: mc,
                strings: state.strings,
            };
            state.chunks = crate::codegen::compile(context, source)?;
            state.define_builtins();
            state.call_function(0)
        })?;
        Ok(())
    }

    pub fn interpret(&mut self) -> Result<ReturnValue, VmError> {
        loop {
            const FUEL_PER_GC: i32 = 1024 * 10;
            let mut fuel = Fuel::new(FUEL_PER_GC);
            // periodically exit the arena in order to collect garbage concurrently with running the VM.
            let result = self.arena.mutate_root(|_, state| state.step(&mut fuel));

            const COLLECTOR_GRANULARITY: f64 = 10240.0;
            if self.arena.metrics().allocation_debt() > COLLECTOR_GRANULARITY {
                // Do garbage collection.
                #[cfg(feature = "debug")]
                println!("Collecting...");
                if self.arena.collection_phase() == CollectionPhase::Sweeping {
                    self.arena.collect_debt();
                } else {
                    // Immediately transition to `CollectionPhase::Sweeping`.
                    self.arena.mark_all().unwrap().start_sweeping();
                }
            }

            match result {
                Ok(result) => {
                    if let Some(value) = result {
                        return Ok(value);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl<'gc> State<'gc> {
    fn runtime_error(&mut self, message: Cow<'static, str>) -> VmError {
        let mut error_message = String::from(message);
        (0..self.frame_count).rev().for_each(|i| {
            let frame = &self.frames[i];
            let function = &frame.closure.function;
            error_message.push_str(&format!(
                "\n[line {}] in ",
                function.chunk.line(frame.ip - 1)
            ));
            let name = if let Some(name) = function.name {
                name.to_str().unwrap()
            } else {
                "script"
            };
            error_message.push_str(name);
            error_message.push('\n');
        });
        VmError::RuntimeError(error_message)
    }

    fn current_frame(&mut self) -> &mut CallFrame<'gc> {
        &mut self.frames[self.frame_count - 1]
    }

    // Dispatch the next opcode, stop at the given frame count.
    // When dispatch in step() function, the stop_at_frame_count is 0.
    // When dispatch in eval_function(), the stop_at_frame_count is the frame count before to call eval_function().
    // This is used to exit the frame call after the chunks of that function is finished.
    fn dispatch_next(
        &mut self,
        stop_at_frame_count: usize,
    ) -> Result<Option<ReturnValue>, VmError> {
        // Debug stack info
        #[cfg(feature = "debug")]
        self.print_stack();
        let frame = self.current_frame();
        // Disassemble instruction for debug
        #[cfg(feature = "debug")]
        frame.disassemble_instruction(frame.ip);
        match frame.next_opcode() {
            OpCode::Constant(byte) => {
                // let byte = frame.read_byte();
                let constant = frame.read_constant(byte);
                self.push_stack(constant);
            }
            OpCode::Add => match (self.peek(0), self.peek(1)) {
                (Value::Number(_), Value::Number(_)) => {
                    binary_op!(self, +);
                }
                (Value::String(_), Value::String(_)) => {
                    let b = self.pop_stack().as_string()?;
                    let a = self.pop_stack().as_string()?;
                    let s = self.intern(format!("{a}{b}").as_bytes());
                    self.push_stack(s.into());
                }
                _ => {
                    return Err(
                        self.runtime_error("Operands must be two numbers or two strings.".into())
                    );
                }
            },
            OpCode::Subtract => {
                binary_op!(self, -);
            }
            OpCode::Multiply => {
                binary_op!(self, *);
            }
            OpCode::Divide => {
                binary_op!(self, /);
            }
            OpCode::Negate => {
                let v = self
                    .pop_stack()
                    .as_number()
                    .map_err(|_| self.runtime_error("Operand must be a number.".into()))?;
                self.push_stack((-v).into());
            }
            OpCode::Return => {
                let frame_slot_start = frame.slot_start;
                let return_value: Value<'_> = self.pop_stack();
                self.close_upvalues(frame_slot_start);
                // Must pop the frame from vec when returning
                self.frames.pop();
                self.frame_count -= 1;
                if self.frame_count == stop_at_frame_count {
                    self.pop_stack();
                    return Ok(Some(return_value.into()));
                }
                self.stack_top = frame_slot_start;
                self.push_stack(return_value);
            }
            OpCode::Nil => self.push_stack(Value::Nil),
            OpCode::True => self.push_stack(Value::Boolean(true)),
            OpCode::False => self.push_stack(Value::Boolean(false)),
            OpCode::Not => {
                let v = self.pop_stack().is_falsy();
                self.push_stack((v).into())
            }
            OpCode::Equal => {
                let b = self.pop_stack();
                let a = self.pop_stack();
                self.push_stack(a.equals(&b).into());
            }
            OpCode::Greater => {
                binary_op!(self, >);
            }
            OpCode::Less => {
                binary_op!(self, <);
            }
            OpCode::Print => {
                let value = self.pop_stack();
                println!("{value}");
            }
            OpCode::Pop => {
                self.pop_stack();
            }
            OpCode::DefineGlobal(byte) => {
                let varible_name = frame.read_constant(byte).as_string()?;
                self.globals.insert(varible_name, *self.peek(0));
                self.pop_stack();
            }
            OpCode::GetGlobal(byte) => {
                let varible_name = frame.read_constant(byte).as_string()?;
                if let Some(value) = self.globals.get(&varible_name) {
                    self.push_stack(*value);
                } else {
                    return Err(self
                        .runtime_error(format!("Undefined variable '{}'.", varible_name).into()));
                }
            }
            OpCode::SetGlobal(byte) => {
                let varible_name = frame.read_constant(byte).as_string()?;
                #[allow(clippy::map_entry)]
                if self.globals.contains_key(&varible_name) {
                    self.globals.insert(varible_name, *self.peek(0));
                } else {
                    return Err(self
                        .runtime_error(format!("Undefined variable '{}'.", varible_name).into()));
                }
            }
            OpCode::GetLocal(slot) => {
                let value = self.stack[frame.slot_start + slot as usize];
                self.push_stack(value);
            }
            OpCode::SetLocal(slot) => {
                let slot_start = frame.slot_start;
                self.stack[slot_start + slot as usize] = *self.peek(0);
            }
            OpCode::JumpIfFalse(offset) => {
                let is_falsy = self.peek(0).is_falsy();
                // Alwasy jump to the next instruction, do not move this line into if block
                if is_falsy {
                    let frame = self.current_frame();
                    frame.ip += offset as usize;
                }
            }
            OpCode::Jump(offset) => {
                frame.ip += offset as usize;
            }
            OpCode::Loop(offset) => {
                frame.ip -= offset as usize;
            }
            OpCode::Call(arg_count) => {
                self.call_value(*self.peek(arg_count as usize), arg_count)?;
            }
            OpCode::Closure(chunk_id) => {
                let function = self.get_chunk(chunk_id as usize)?;
                let mut closure = Closure::new(self.mc, function);

                closure
                    .function
                    .upvalues
                    .iter()
                    .enumerate()
                    .for_each(|(i, upvalue)| {
                        let frame = self.current_frame();
                        let Upvalue { is_local, index } = *upvalue;
                        if is_local {
                            let slot = frame.slot_start + index;
                            let upvalue = self.capture_upvalue(slot);
                            // println!("function {} capture local: {slot}, {:?}", fn_name, upvalue);
                            closure.upvalues[i] = upvalue;
                        } else {
                            // println!(
                            //     "function {} capture upvalue: {index} {:?}",
                            //     fn_name, &frame.closure.upvalues[index]
                            // );
                            closure.upvalues[i] = frame.closure.upvalues[index];
                        }
                    });

                self.push_stack(Value::from(Gc::new(self.mc, closure)));
            }
            OpCode::GetUpvalue(slot) => {
                let slot = slot as usize;
                let upvalue = frame.closure.upvalues[slot];
                if let Some(closed) = upvalue.borrow().closed {
                    self.push_stack(closed);
                } else {
                    let location = frame.closure.upvalues[slot].borrow().location;
                    let upvalue = self.stack[location];
                    self.push_stack(upvalue);
                }
            }
            OpCode::SetUpvalue(slot) => {
                let slot = slot as usize;
                let mut upvalue = frame.closure.upvalues[slot].borrow_mut(self.mc);
                let stack_position = upvalue.location;
                upvalue.location = slot;

                let value = *self.peek(slot);
                upvalue.closed = Some(value);
                // Also update the stack value
                self.stack[stack_position] = value;
            }
            OpCode::CloseUpvalue => {
                self.close_upvalues(self.stack_top - 1);
                self.pop_stack();
            }
            OpCode::Class(byte) => {
                let name = frame.read_constant(byte).as_string().unwrap();
                self.push_stack(Value::from(Gc::new(
                    self.mc,
                    RefLock::new(Class::new(name)),
                )));
            }
            OpCode::GetProperty(byte) => {
                if let Ok(instance) = self.peek(0).as_instance() {
                    let frame = self.current_frame();
                    let name = frame.read_constant(byte).as_string().unwrap();
                    if let Some(property) = instance.borrow().fields.get(&name) {
                        self.pop_stack(); // Instance
                        self.push_stack(*property);
                    } else {
                        self.bind_method(instance.borrow().class, name)?;
                    }
                } else {
                    return Err(self.runtime_error("Only instances have properties.".into()));
                }
            }
            OpCode::SetProperty(byte) => {
                if let Ok(instantce) = self.peek(1).as_instance() {
                    let value = *self.peek(0);
                    let frame = self.current_frame();
                    let name = frame.read_constant(byte).as_string().unwrap();
                    instantce.borrow_mut(self.mc).fields.insert(name, value);

                    let value = self.pop_stack(); // Value
                    self.pop_stack(); // Instance
                    self.push_stack(value);
                } else {
                    return Err(self.runtime_error("Only instances have fields.".into()));
                }
            }
            OpCode::Method(byte) => {
                let name = frame.read_constant(byte).as_string().unwrap();
                self.define_method(name);
            }
            OpCode::Invoke(byte, arg_count) => {
                let method_name = frame.read_constant(byte).as_string().unwrap();
                self.invoke(method_name, arg_count)?;
            }
            OpCode::Inherit => {
                if let Value::Class(superclass) = self.peek(1) {
                    let subclass = self.peek(0).as_class()?;
                    subclass
                        .borrow_mut(self.mc)
                        .methods
                        .extend(&superclass.borrow().methods);
                    self.pop_stack(); // Subclass
                } else {
                    return Err(self.runtime_error("Superclass must be a class.".into()));
                }
            }
            OpCode::GetSuper(byte) => {
                let name = frame.read_constant(byte).as_string().unwrap();
                let superclass = self.pop_stack().as_class()?;
                self.bind_method(superclass, name)?
            }
            OpCode::SuperInvoke(byte, arg_count) => {
                let method_name = frame.read_constant(byte).as_string().unwrap();
                let superclass = self.pop_stack().as_class()?;
                self.invoke_from_class(superclass, method_name, arg_count)?;
            }
            OpCode::Prompt => {
                let message = self.pop_stack().as_string().unwrap().to_string();
                let result = Value::from(self.intern(ai::prompt(message).as_bytes()));
                self.push_stack(result);
            }
            OpCode::Agent(name) => {
                let agent = frame.read_constant(name);
                self.push_stack(agent);
            }
        }
        Ok(None)
    }

    pub fn eval_function(&mut self, chunk_id: usize) -> Result<ReturnValue, VmError> {
        // Remember the current frame count in order to exit the loop at the correct frame.
        let frame_count = self.frame_count;
        self.call_function(chunk_id)?;
        loop {
            if let Some(result) = self.dispatch_next(frame_count)? {
                return Ok(result);
            }
        }
    }

    // Runs the VM for a period of time controlled by the `fuel` parameter.
    //
    // Returns `Ok(false)` if the method has exhausted its fuel, but there is more work to
    // do, and returns `Ok(true)` if no more progress can be made.
    fn step(&mut self, fuel: &mut Fuel) -> Result<Option<ReturnValue>, VmError> {
        loop {
            if let Some(result) = self.dispatch_next(0)? {
                return Ok(Some(result));
            }
            const FUEL_PER_STEP: i32 = 1;
            fuel.consume(FUEL_PER_STEP);

            if !fuel.should_continue() {
                return Ok(None);
            }
        }
    }

    fn capture_upvalue(&mut self, slot: usize) -> GcRefLock<'gc, UpvalueObj<'gc>> {
        let mut prev_upvalue = None;
        let mut open_upvalue = self.open_upvalues;
        while open_upvalue.map(|u| u.borrow().location > slot) == Some(true) {
            if let Some(upvalue) = open_upvalue {
                prev_upvalue = Some(upvalue);
                open_upvalue = upvalue.borrow().next;
            }
        }
        if let Some(upvalue) = open_upvalue {
            if upvalue.borrow().location == slot {
                // We found an existing upvalue capturing the variable,
                // so we reuse that upvalue.
                return upvalue;
            }
        }

        // Do not use peek() to get value! the slot would be incorrect offset to peek.
        // let local = &self.stack[slot].clone();
        // create a new upvalue for our local slot and insert it into the list at the right location.
        let created_upvalue = Gc::new(
            self.mc,
            RefLock::new(UpvalueObj {
                location: slot,
                closed: None,
                next: open_upvalue,
            }),
        );
        if let Some(prev) = prev_upvalue {
            prev.borrow_mut(self.mc).next = Some(created_upvalue);
        } else {
            self.open_upvalues = Some(created_upvalue);
        }
        created_upvalue
    }

    fn close_upvalues(&mut self, last: usize) {
        loop {
            if self.open_upvalues.map(|u| u.borrow().location < last) == Some(true) {
                break;
            }

            if let Some(upvalue) = self.open_upvalues.take() {
                let mut upvalue = upvalue.borrow_mut(self.mc);
                let local = self.stack[upvalue.location];
                upvalue.closed = Some(local);
                // Dummy location after closed assigned
                // In C's version, the location is a pointer to upvalue.closed
                // upvalue.location = 0;
                self.open_upvalues = upvalue.next;
            } else {
                break;
            }
        }
    }

    fn define_method(&mut self, name: InternedString<'gc>) {
        let class = self.peek(1).as_class().unwrap();
        class
            .borrow_mut(self.mc)
            .methods
            .insert(name, *self.peek(0));
        // pop the closure since we’re done with it.
        self.pop_stack();
    }

    pub fn define_builtins(&mut self) {
        self.define_native_function("clock", builtins::clock);
    }

    fn define_native_function(&mut self, name: &'static str, function: NativeFn<'gc>) {
        let s = self.intern_static(name);
        self.globals.insert(s, Value::NativeFunction(function));
    }

    fn bind_method(
        &mut self,
        class: GcRefLock<'gc, Class<'gc>>,
        name: InternedString<'gc>,
    ) -> Result<(), VmError> {
        if let Some(method) = class.borrow().methods.get(&name) {
            let bound = BoundMethod::new(*self.peek(0), (*method).as_closure()?);
            // pop the instance and replace the top of
            // the stack with the bound method.
            self.pop_stack();
            self.push_stack(Value::from(Gc::new(self.mc, bound)));
            Ok(())
        } else {
            Err(self.runtime_error(format!("Undefined property '{}'.", name).into()))
        }
    }

    fn call_value(&mut self, callee: Value<'gc>, arg_count: u8) -> Result<(), VmError> {
        match callee {
            Value::BoundMethod(bound) => {
                // inserts the receiver into the new CallFrame's slot zero.
                // normally, the receiver is 'this' or 'super' keyword.
                /*
                   Diagram for this: scone.topping("berries", "cream");

                                                   stackTop
                                                       |
                    <-- -1 --> <------ argCount ---->  |
                       0         1         2         3 v
                       |         |         |         |
                       v         v         v         v
                   +----------+-----------+-----------+---
                   | script   |fn topping()| "berries" | "cream"
                   +----------+-----------+-----------+---
                       ^                               ^
                       |                               |
                       +-------------------------------+
                           topping Callframe
                */
                self.stack[self.stack_top - arg_count as usize - 1] = bound.receiver;
                return self.call(bound.method, arg_count);
            }
            Value::Class(class) => {
                let instance = Instance::new(class);
                self.stack[self.stack_top - arg_count as usize - 1] =
                    Value::from(Gc::new(self.mc, RefLock::new(instance)));
                let init = self.intern_static("init");
                if let Some(initializer) = class.borrow().methods.get(&init) {
                    return self.call(initializer.as_closure()?, arg_count);
                } else if arg_count != 0 {
                    return Err(self.runtime_error(
                        format!("Expected 0 arguments but got {}.", arg_count).into(),
                    ));
                }
            }
            Value::Closure(closure) => return self.call(closure, arg_count),
            Value::NativeFunction(function) => {
                let result = function(self.pop_stack_n(arg_count as usize));
                // Stack should be restored after native function called
                self.stack_top -= 1;
                self.push_stack(result);
            }
            value @ Value::Agent(..) => {
                // Run the agent with given input message
                self.push_stack(value);
            }
            _ => {
                return Err(self.runtime_error("Can only call functions and classes.".into()));
            }
        }
        Ok(())
    }

    fn invoke_from_class(
        &mut self,
        class: GcRefLock<'gc, Class<'gc>>,
        name: InternedString<'gc>,
        arg_count: u8,
    ) -> Result<(), VmError> {
        if let Some(method) = class.borrow().methods.get(&name) {
            self.call(method.as_closure()?, arg_count)
        } else {
            Err(self.runtime_error(format!("Undefined property '{}'.", name).into()))
        }
    }

    fn invoke(&mut self, name: InternedString<'gc>, arg_count: u8) -> Result<(), VmError> {
        let receiver = *self.peek(arg_count as usize);
        if let Value::Instance(instance) = receiver {
            if let Some(value) = instance.borrow().fields.get(&name) {
                self.stack[self.stack_top - arg_count as usize - 1] = *value;
                self.call_value(*value, arg_count)
            } else {
                self.invoke_from_class(instance.borrow().class, name, arg_count)
            }
        } else if let Value::Agent(agent) = receiver {
            if name == "run" {
                let message = self.pop_stack();
                let result = ai::run_agent(self, agent, message.as_string().unwrap());
                let s = self.intern(result.as_bytes());
                self.push_stack(Value::from(s));
                Ok(())
            } else {
                Err(self.runtime_error(format!("Agent have no methods called '{}'.", name).into()))
            }
        } else {
            Err(self.runtime_error("Only instances have methods.".into()))
        }
    }

    fn call(&mut self, closure: Gc<'gc, Closure<'gc>>, arg_count: u8) -> Result<(), VmError> {
        // #[cfg(feature = "debug")]
        // closure.function.disassemble("fn");
        if arg_count != closure.function.arity {
            return Err(self.runtime_error(
                format!(
                    "Expected {} arguments but got {}.",
                    closure.function.arity, arg_count
                )
                .into(),
            ));
        }
        if self.frame_count == FRAME_MAX_SIZE {
            return Err(self.runtime_error("Stack overflow.".into()));
        }

        let call_frame = CallFrame {
            closure,
            ip: 0,
            slot_start: self.stack_top - arg_count as usize - 1,
        };
        // self.frames[self.frame_count] = call_frame;
        self.frames.push(call_frame);
        self.frame_count += 1;
        Ok(())
    }

    #[inline]
    fn stack_get(&mut self, index: usize) -> Value<'gc> {
        unsafe { *self.stack.get_unchecked(index) }
    }

    #[inline]
    fn stack_set(&mut self, index: usize, value: Value<'gc>) {
        unsafe { *self.stack.get_unchecked_mut(index) = value }
    }

    #[inline]
    fn push_stack(&mut self, value: Value<'gc>) {
        self.stack_set(self.stack_top, value);
        self.stack_top += 1;
    }

    #[inline]
    fn pop_stack(&mut self) -> Value<'gc> {
        self.stack_top -= 1;
        self.stack_get(self.stack_top)
    }

    fn pop_stack_n(&mut self, n: usize) -> Vec<Value<'gc>> {
        if n == 0 {
            return Vec::new();
        }

        // Ensure we don't pop more items than are on the stack
        let n = n.min(self.stack_top);

        let new_top = self.stack_top - n;
        let mut result = Vec::with_capacity(n);

        // Copy values from the stack to the result vector
        result.extend_from_slice(&self.stack[new_top..self.stack_top]);

        // Update the stack top
        self.stack_top = new_top;

        // No need to reverse as we're copying from bottom to top
        result
    }

    #[inline]
    fn peek(&self, distance: usize) -> &Value<'gc> {
        &self.stack[self.stack_top - 1 - distance]
    }

    #[cfg(feature = "debug")]
    fn print_stack(&self) {
        print!("          ");
        for value in self.stack.iter().take(self.stack_top) {
            print!("[ ");
            print!("{value}");
            print!(" ]")
        }
        println!();
    }
}

impl Vm {
    pub fn inject_variables(&mut self, variables: HashMap<String, serde_json::Value>) {
        self.arena.mutate_root(|_mc, state| {
            for (key, value) in variables {
                let name = state.intern(key.as_bytes());
                let v = match value {
                    serde_json::Value::Bool(b) => Value::Boolean(b),
                    serde_json::Value::Number(number) => Value::Number(number.as_f64().unwrap()),
                    serde_json::Value::String(str) => {
                        let s = state.intern(str.as_bytes());
                        Value::String(s)
                    }
                    serde_json::Value::Null => Value::Nil,
                    _ => continue,
                };
                state.globals.insert(name, v);
            }
        });
    }

    pub fn inject_instance(
        &mut self,
        name: &'static str,
        fields: HashMap<&'static str, serde_json::Value>,
    ) {
        self.arena.mutate_root(|mc, state| {
            let name = state.intern_static(name);
            let class = Gc::new(mc, RefLock::new(Class::new(name)));
            let mut instance = Instance::new(class);
            for (key, value) in fields {
                let v = match value {
                    serde_json::Value::Bool(b) => Value::Boolean(b),
                    serde_json::Value::Number(number) => Value::Number(number.as_f64().unwrap()),
                    serde_json::Value::String(str) => {
                        let s = state.intern(str.as_bytes());
                        Value::from(s)
                    }
                    serde_json::Value::Null => Value::Nil,
                    _ => continue,
                };
                instance.fields.insert(state.intern_static(key), v);
            }
            state
                .globals
                .insert(name, Gc::new(mc, RefLock::new(instance)).into());
        });
    }
}

#[derive(Copy, Clone)]
pub struct Context<'gc> {
    pub mutation: &'gc Mutation<'gc>,
    pub strings: InternedStringSet<'gc>,
}

impl<'gc> Context<'gc> {
    pub fn intern(self, s: &[u8]) -> InternedString<'gc> {
        self.strings.intern(&self, s)
    }

    #[allow(unused)]
    pub fn intern_static(self, s: &'static str) -> InternedString<'gc> {
        self.strings.intern_static(&self, s.as_bytes())
    }
}

impl<'gc> ops::Deref for Context<'gc> {
    type Target = Mutation<'gc>;

    fn deref(&self) -> &Self::Target {
        self.mutation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_variables() {
        let mut vm = Vm::new();
        vm.inject_variables({
            let mut map = HashMap::new();
            map.insert("test".into(), "abc".into());
            map.insert("test2".into(), 123.into());
            map.insert("test3".into(), true.into());
            map
        });
        vm.compile("return test;").unwrap();
        let result = vm.interpret().unwrap();
        assert_eq!(result, ReturnValue::String("abc".into()));
        vm.compile("return test2;").unwrap();
        let result = vm.interpret().unwrap();
        assert_eq!(result, ReturnValue::Number(123.0));
        vm.compile("return test3;").unwrap();
        let result = vm.interpret().unwrap();
        assert_eq!(result, ReturnValue::Boolean(true));
    }

    #[test]
    fn test_inject_instance() {
        let mut vm = Vm::new();
        vm.inject_instance("request", {
            let mut map = HashMap::new();
            map.insert("method", "get".into());
            map.insert("code", 200.0.into());
            map.insert("test", true.into());
            map
        });
        vm.compile("return request;").unwrap();
        let result = vm.interpret().unwrap();
        let request = result.as_object().unwrap();
        assert_eq!(request.get("method").unwrap(), "get");
        assert_eq!(request.get("code").unwrap(), 200.0);
        assert_eq!(request.get("test").unwrap(), true);
        assert!(request.get("abc").is_none());
    }
}
