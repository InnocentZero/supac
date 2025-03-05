use std::path::Path;

use anyhow::Result;
use nu_cli::gather_parent_env_vars;
use nu_cmd_lang::create_default_context;
use nu_command::add_shell_command_context;
use nu_engine::eval_block_with_early_return;
use nu_protocol::{
    PipelineData::Empty,
    Record, Span,
    debugger::WithoutDebug,
    engine::{Closure, EngineState, Stack, StateWorkingSet},
};

pub struct Engine {
    engine: EngineState,
    stack: Stack,
}

impl Engine {
    pub fn new(config_dir: &Path) -> Self {
        let mut engine_state = create_default_context();
        engine_state = add_shell_command_context(engine_state);
        gather_parent_env_vars(&mut engine_state, config_dir);

        let stack = Stack::new();

        Engine {
            engine: engine_state,
            stack,
        }
    }

    pub fn fetch(&mut self, contents: &[u8]) -> Result<Record> {
        let mut working_set = StateWorkingSet::new(&self.engine);
        let block = nu_parser::parse(&mut working_set, None, contents, false);

        self.engine.merge_delta(working_set.render())?;

        eval_block_with_early_return::<WithoutDebug>(&self.engine, &mut self.stack, &block, Empty)
            .map(|pipeline_data| -> Result<_> {
            Ok(pipeline_data
                .into_value(Span::test_data())?
                .as_record()?
                .to_owned())
        })?
    }

    pub fn execute_closure(&mut self, closure: &Closure) -> Result<()> {
        eval_block_with_early_return::<WithoutDebug>(
            &self.engine,
            &mut self.stack,
            self.engine.get_block(closure.block_id),
            Empty,
        )
        .map(|_| Ok(()))?
    }
}
