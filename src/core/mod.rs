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
pub mod layout;
pub mod live;
pub mod measure;
pub mod pager;
pub mod registry;
pub mod retain;
pub mod schedule;
pub mod snapshot;
pub mod spark;
pub mod table;
// The template and variable layers land bottom-up: their evaluation
// halves (substitute/expand, the resolvers, the partial evaluator)
// gain production callers when the shell runner and the check command
// land; until then only the KDL walk consumes them. Remove each allow
// with that first caller.
#[allow(dead_code)]
pub mod template;
pub mod trigger;
#[allow(dead_code)]
pub mod variables;
