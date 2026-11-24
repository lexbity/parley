pub(crate) mod coach;
pub(crate) mod dispatch;
pub(crate) mod probe;
pub(crate) mod profile;
mod protocol;
mod relay;
mod supervisor;

fn coach_schema() -> &'static [u8] {
    crate::application::coach::SCHEMA.as_bytes()
}
