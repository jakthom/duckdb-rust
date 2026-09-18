//! Provisional bounded scalar-expression expansion, not a macro catalog or
//! stored-expression encoding. Ordered nodes avoid recursive foreign payloads.
use crate::{
    common::{Error, Result},
    parallel::QueryContext,
};

#[derive(Debug, Clone)]
pub enum ScalarExpansionNode {
    Argument(usize),
    Null,
    Equal {
        left: usize,
        right: usize,
    },
    Case {
        condition: usize,
        then_value: usize,
        otherwise: usize,
    },
}

/// Nodes are in child-before-parent order and the root is the final node.
/// Reusing an index denotes another expression occurrence, not a cached value.
#[derive(Debug, Clone)]
pub struct ScalarExpansion {
    pub nodes: Vec<ScalarExpansionNode>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarExpansion {
    pub fn validate(&self, arity: usize, query: &QueryContext) -> Result<()> {
        query.check()?;
        if self.nodes.is_empty() {
            return Err(Error::Bind("empty scalar expansion".into()));
        }
        // Limits apply to a supplied template and its expanded occurrences,
        // not to the range of any SQL scalar type or mathematical value.
        if self.nodes.len() > 1024 {
            return Err(Error::Resource("scalar expansion node limit".into()));
        }
        let mut depths = Vec::with_capacity(self.nodes.len());
        let mut occurrences = Vec::with_capacity(self.nodes.len());
        for (index, node) in self.nodes.iter().enumerate() {
            query.check()?;
            let children: &[usize] = match node {
                ScalarExpansionNode::Argument(argument) => {
                    if *argument >= arity {
                        return Err(Error::Bind(
                            "scalar expansion argument outside signature".into(),
                        ));
                    }
                    &[]
                }
                ScalarExpansionNode::Null => &[],
                ScalarExpansionNode::Equal { left, right } => &[*left, *right],
                ScalarExpansionNode::Case {
                    condition,
                    then_value,
                    otherwise,
                } => &[*condition, *then_value, *otherwise],
            };
            let mut depth = 1usize;
            let mut count = 1usize;
            for &child in children {
                if child >= index {
                    return Err(Error::Bind(
                        "scalar expansion children must precede parents".into(),
                    ));
                }
                depth = depth.max(depths[child] + 1);
                count += occurrences[child];
            }
            if depth > 64 || count > 4096 {
                return Err(Error::Resource(
                    "scalar expansion occurrence/depth limit".into(),
                ));
            }
            depths.push(depth);
            occurrences.push(count);
        }
        Ok(())
    }
}
