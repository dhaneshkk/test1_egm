//! egm-core: the Elephant-Goldfish workflow as a library. The `egm-dsh` CLI and the
//! `egm-team` web app are thin layers over it.

pub mod agent;
pub mod alarm;
pub mod commands;
pub mod cost;
pub mod criteria;
pub mod demo;
pub mod diagram;
pub mod doc;
pub mod escalate;
pub mod estimate;
pub mod evidence;
pub mod gate;
pub mod executor;
pub mod fix;
pub mod goldfish;
pub mod metrics;
pub mod official;
pub mod phase;
pub mod lock;
pub mod project;
pub mod run;
pub mod safety;
pub mod scope;
pub mod session;
pub mod spec;
pub mod stage;
pub mod steps;
pub mod triage;
pub mod tuning;
pub mod verify;
pub mod vcs;
pub mod ui;
pub mod events;
pub mod policy;
pub mod queue;
