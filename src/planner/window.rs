use super::{BoundExpr, logical::OrderExpr};
use crate::{
    common::DataType,
    function::window::{WindowFunction, WindowOptions},
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameUnits {
    Rows,
    Range,
    Groups,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameBound {
    UnboundedPreceding,
    Preceding(usize),
    CurrentRow,
    Following(usize),
    UnboundedFollowing,
}
#[derive(Clone, Copy, Debug)]
pub struct WindowFrame {
    pub units: FrameUnits,
    pub start: FrameBound,
    pub end: FrameBound,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowFrame {
    /// Shared by the SQL binder and logical-plan validation for other frontends.
    /// Within-category reversed offsets are valid and produce empty frames.
    pub fn validate(&self) -> crate::Result<()> {
        use FrameBound::*;
        if self.start == UnboundedFollowing
            || self.end == UnboundedPreceding
            || matches!(
                (self.start, self.end),
                (Following(_), CurrentRow | Preceding(_)) | (CurrentRow, Preceding(_))
            )
        {
            return Err(crate::Error::Bind("invalid window frame bounds".into()));
        }
        if self.units == FrameUnits::Range
            && [self.start, self.end]
                .iter()
                .any(|bound| matches!(bound, Preceding(_) | Following(_)))
        {
            return Err(crate::Error::Unsupported(
                "RANGE frames with value offsets".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct WindowExpression {
    pub function: Arc<dyn WindowFunction>,
    pub arguments: Vec<BoundExpr>,
    pub partition: Vec<BoundExpr>,
    pub order: Vec<OrderExpr>,
    pub frame: WindowFrame,
    pub options: WindowOptions,
    pub filter: Option<BoundExpr>,
    pub data_type: DataType,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowExpression {
    pub fn visit_expressions(&self, visit: &mut impl FnMut(&BoundExpr)) {
        for expr in self
            .arguments
            .iter()
            .chain(&self.partition)
            .chain(self.filter.iter())
        {
            visit(expr);
        }
        for key in &self.order {
            visit(&key.expression);
        }
    }
    pub fn map_expressions(
        mut self,
        map: &mut impl FnMut(BoundExpr) -> crate::Result<BoundExpr>,
    ) -> crate::Result<Self> {
        self.arguments = self
            .arguments
            .into_iter()
            .map(&mut *map)
            .collect::<crate::Result<_>>()?;
        self.partition = self
            .partition
            .into_iter()
            .map(&mut *map)
            .collect::<crate::Result<_>>()?;
        self.filter = self.filter.map(&mut *map).transpose()?;
        self.order = self
            .order
            .into_iter()
            .map(|mut key| {
                key.expression = map(key.expression)?;
                Ok(key)
            })
            .collect::<crate::Result<_>>()?;
        Ok(self)
    }
}
