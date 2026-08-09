pub mod bar;
pub mod box_model;
pub mod child;
pub mod dashboard_file;
pub mod dashboard_kdl;
pub mod datetime;
pub mod decode;
pub mod duration;
pub mod fuzzy;
pub mod join;
// Consumed by the dashboard walk's `key` node; until that node lands,
// only tests read this module.
#[allow(dead_code)]
pub mod key_spelling;
pub mod layout;
pub mod live;
pub mod measure;
pub mod pager;
pub mod registry;
pub mod retain;
pub mod schedule;
pub mod shell;
pub mod snapshot;
pub mod spark;
pub mod table;
pub mod template;
pub mod trigger;
pub mod variables;
