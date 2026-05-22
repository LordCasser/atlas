//! React framework resolver.

use crate::frameworks::FrameworkResolver;
use atlas_types::*;

pub struct ReactResolver;

impl FrameworkResolver for ReactResolver {
    fn framework_name(&self) -> &str {
        "react"
    }

    fn supported_edge_kinds(&self) -> &[EdgeKind] {
        &[
            EdgeKind::Calls,
            EdgeKind::References,
            EdgeKind::Instantiates,
        ]
    }
}
