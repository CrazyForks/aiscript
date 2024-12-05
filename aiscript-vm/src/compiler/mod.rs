use std::collections::BTreeMap;

use codegen::CodeGen;
use gc_arena::Gc;

use crate::{ast::ChunkId, object::Function, parser::Parser, vm::Context, VmError};

mod codegen;

pub fn compile<'gc>(
    ctx: Context<'gc>,
    source: &'gc str,
) -> Result<BTreeMap<ChunkId, Gc<'gc, Function<'gc>>>, VmError> {
    let mut parser = Parser::new(ctx, source);
    let program = parser.parse()?;
    #[cfg(feature = "debug")]
    println!("AST: {}", program);
    CodeGen::generate(program, ctx).map(|chunks| {
        chunks
            .into_iter()
            .map(|(id, function)| (id, Gc::new(&ctx, function)))
            .collect()
    })
}
