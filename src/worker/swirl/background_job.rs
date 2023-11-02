use crate::schema::background_jobs;
use crate::worker::swirl::errors::EnqueueError;
use crate::worker::swirl::perform_state::PerformState;
use crate::worker::swirl::PerformError;
use diesel::prelude::*;
use diesel::PgConnection;
use serde::de::DeserializeOwned;
use serde::Serialize;

pub trait BackgroundJob: Serialize + DeserializeOwned + 'static {
    /// Unique name of the task.
    ///
    /// This MUST be unique for the whole application.
    const JOB_NAME: &'static str;

    /// Default priority of the task.
    ///
    /// [Self::enqueue_with_priority] can be used to override the priority value.
    const PRIORITY: i16 = 0;

    /// The application data provided to this job at runtime.
    type Context: Clone + Send + 'static;

    /// Execute the task. This method should define its logic
    fn run(&self, state: PerformState<'_>, env: &Self::Context) -> Result<(), PerformError>;

    fn enqueue(&self, conn: &mut PgConnection) -> Result<(), EnqueueError> {
        self.enqueue_with_priority(conn, Self::PRIORITY)
    }

    #[instrument(name = "swirl.enqueue", skip(self, conn), fields(message = Self::JOB_NAME))]
    fn enqueue_with_priority(
        &self,
        conn: &mut PgConnection,
        job_priority: i16,
    ) -> Result<(), EnqueueError> {
        let job_data = serde_json::to_value(self)?;
        diesel::insert_into(background_jobs::table)
            .values((
                background_jobs::job_type.eq(Self::JOB_NAME),
                background_jobs::data.eq(job_data),
                background_jobs::priority.eq(job_priority),
            ))
            .execute(conn)?;
        Ok(())
    }
}
