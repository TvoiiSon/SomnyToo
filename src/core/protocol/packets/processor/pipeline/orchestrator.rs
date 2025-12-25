use tracing::info;

use crate::core::protocol::packets::processor::pipeline::stages::common::{PipelineStage, PipelineContext, StageError};

pub struct PipelineOrchestrator {
    stages: Vec<Box<dyn PipelineStage>>,
}

impl PipelineOrchestrator {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn add_stage<S: PipelineStage + 'static>(mut self, stage: S) -> Self {
        self.stages.push(Box::new(stage));
        self
    }

    pub async fn execute(&self, mut context: PipelineContext) -> Result<Vec<u8>, StageError> {
        let start_time = std::time::Instant::now();

        for (i, stage) in self.stages.iter().enumerate() {
            info!("Executing pipeline stage {}", i + 1);
            stage.execute(&mut context).await?;
        }

        let processing_time = start_time.elapsed();
        info!("Pipeline execution completed in {:?}", processing_time);

        context.encrypted_response
            .ok_or_else(|| StageError::ProcessingFailed("No response generated".to_string()))
    }
}