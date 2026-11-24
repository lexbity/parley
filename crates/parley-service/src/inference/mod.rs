pub(crate) mod coach;
pub(crate) mod dispatch;
pub(crate) mod probe;
pub(crate) mod profile;
pub(crate) mod protocol;
mod relay;
pub(crate) mod roles;
pub(crate) mod supervisor;
pub(crate) mod template_builder;

pub(crate) fn coach_schema() -> &'static [u8] {
    crate::application::coach::SCHEMA.as_bytes()
}

pub(crate) fn builder_schema() -> &'static [u8] {
    crate::application::template_build::SCHEMA.as_bytes()
}

pub(crate) fn interviewer_schema() -> &'static [u8] {
    crate::jobs::interviewer_schema().as_bytes()
}

pub(crate) fn briefer_schema() -> &'static [u8] {
    crate::jobs::briefer_schema().as_bytes()
}
