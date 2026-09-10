use super::*;
use crate::{
    function::window::{NullTreatment, WindowOptions},
    planner::window::{FrameBound, FrameUnits, WindowExpression, WindowFrame},
};

pub(super) struct Windows {
    named: BTreeMap<String, ast::WindowSpec>,
    pub calls: Vec<(ast::Expr, ast::WindowSpec)>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Windows {
    pub fn new(definitions: &[ast::NamedWindowDefinition]) -> Result<Self> {
        let mut result = Self {
            named: BTreeMap::new(),
            calls: Vec::new(),
        };
        for ast::NamedWindowDefinition(name, expression) in definitions {
            let key = name.value.to_ascii_lowercase();
            if result.named.contains_key(&key) {
                return Err(Error::Bind(format!("window {name} is already defined")));
            }
            let spec = match expression {
                ast::NamedWindowExpr::WindowSpec(spec) => result.spec(spec)?,
                ast::NamedWindowExpr::NamedWindow(name) => result.named(name)?.clone(),
            };
            result.named.insert(key, spec);
        }
        Ok(result)
    }
    fn named(&self, name: &ast::Ident) -> Result<&ast::WindowSpec> {
        self.named
            .get(&name.value.to_ascii_lowercase())
            .ok_or_else(|| Error::Bind(format!("window {name} does not exist")))
    }
    fn spec(&self, spec: &ast::WindowSpec) -> Result<ast::WindowSpec> {
        let Some(name) = &spec.window_name else {
            return Ok(spec.clone());
        };
        let base = self.named(name)?;
        if !spec.partition_by.is_empty() {
            return Err(Error::Bind(format!(
                "Cannot override PARTITION BY clause of window \"{name}\""
            )));
        }
        if !spec.order_by.is_empty() && !base.order_by.is_empty() {
            return Err(Error::Bind(format!(
                "Cannot override ORDER BY clause of window \"{name}\""
            )));
        }
        if base.window_frame.is_some() {
            return Err(Error::Bind("cannot copy window with a frame clause".into()));
        }
        Ok(ast::WindowSpec {
            window_name: None,
            partition_by: base.partition_by.clone(),
            order_by: if spec.order_by.is_empty() {
                base.order_by.clone()
            } else {
                spec.order_by.clone()
            },
            window_frame: spec.window_frame.clone(),
        })
    }
    pub fn gather(&mut self, expression: &ast::Expr) -> Result<()> {
        visit_expression(expression, &mut |expression| {
            if let ast::Expr::Function(function) = expression
                && let Some(over) = &function.over
            {
                let spec = match over {
                    ast::WindowType::WindowSpec(spec) => self.spec(spec)?,
                    ast::WindowType::NamedWindow(name) => self.named(name)?.clone(),
                };
                if !self.calls.iter().any(|(expr, _)| expr == expression) {
                    self.calls.push((expression.clone(), spec));
                }
                return Ok(false);
            }
            Ok(true)
        })
    }
}

/// Window calls bind in expression order. Pure calls may share a result;
/// effectful calls always retain their own operation and result column.
pub(super) struct WindowScope {
    input_width: usize,
    definitions: Windows,
    outputs: RefCell<Vec<(ast::Expr, WindowExpression)>>,
    shared: RefCell<Vec<(ast::Expr, BoundExpr)>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowScope {
    pub fn new(input_width: usize, definitions: Windows) -> Self {
        Self {
            input_width,
            definitions,
            outputs: RefCell::new(Vec::new()),
            shared: RefCell::new(Vec::new()),
        }
    }
    pub fn bind(
        &self,
        state: &State<'_, '_>,
        expression: &ast::Expr,
        fields: &Scope,
        grouping: Option<&GroupScope>,
    ) -> Result<BoundExpr> {
        if let Some((_, bound)) = self
            .shared
            .borrow()
            .iter()
            .find(|(expr, _)| expr == expression)
        {
            return Ok(bound.clone());
        }
        let ast::Expr::Function(function) = expression else {
            unreachable!()
        };
        let spec = match function.over.as_ref().expect("window call") {
            ast::WindowType::WindowSpec(spec) => self.definitions.spec(spec)?,
            ast::WindowType::NamedWindow(name) => self.definitions.named(name)?.clone(),
        };
        let mut inputs = fields.clone();
        inputs.windows = None;
        let window = state.window(expression, &spec, &inputs, grouping)?;
        let effects = window.function.effects();
        let mut effectful = effects.volatile || effects.external_access;
        window.visit_expressions(&mut |expr| effectful |= has_effects(expr));
        let result = BoundExpr::column(
            self.input_width + self.outputs.borrow().len(),
            window.data_type.clone(),
        );
        self.outputs.borrow_mut().push((expression.clone(), window));
        if !effectful {
            self.shared
                .borrow_mut()
                .push((expression.clone(), result.clone()));
        }
        Ok(result)
    }
    pub fn is_empty(&self) -> bool {
        self.outputs.borrow().is_empty()
    }
    pub fn take(&self) -> Vec<(ast::Expr, WindowExpression)> {
        std::mem::take(&mut *self.outputs.borrow_mut())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn has_effects(expression: &BoundExpr) -> bool {
    let mut effectful = match &expression.kind {
        ExprKind::Scalar(function, _) => {
            let effects = function.effects();
            effects.volatile || effects.external_access
        }
        ExprKind::Operator(function, _) => {
            let effects = function.effects();
            effects.volatile || effects.external_access
        }
        ExprKind::Subquery(query) => plan_effects(&query.plan),
        _ => false,
    };
    expression.visit_children(&mut |expr| effectful |= has_effects(expr));
    effectful
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn plan_effects(plan: &LogicalPlan) -> bool {
    let mut effectful = false;
    plan.visit_expressions(&mut |expr| effectful |= has_effects(expr));
    plan.visit_inputs(&mut |plan| effectful |= plan_effects(plan));
    if let PlanNode::Window { expressions, .. } = &plan.node {
        effectful |= expressions.iter().any(|window| {
            let effects = window.function.effects();
            effects.volatile || effects.external_access
        });
    }
    effectful
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn collect_aggregates(
        &self,
        expression: &ast::Expr,
        fields: &Scope,
        grouping: &GroupScope,
    ) -> Result<()> {
        visit_expression(expression, &mut |expr| {
            if self.is_aggregate(expr) {
                self.expr(expr, fields, Some(grouping))?;
                return Ok(false);
            }
            Ok(true)
        })
    }
    pub(super) fn is_aggregate(&self, expression: &ast::Expr) -> bool {
        matches!(expression, ast::Expr::Function(f) if f.over.is_none() && (self.context.functions.aggregate(&f.name.to_string()).is_some() || f.name.to_string().eq_ignore_ascii_case("grouping") || f.name.to_string().eq_ignore_ascii_case("grouping_id")))
    }
    pub(super) fn window(
        &self,
        expression: &ast::Expr,
        spec: &ast::WindowSpec,
        fields: &Scope,
        grouping: Option<&GroupScope>,
    ) -> Result<WindowExpression> {
        let ast::Expr::Function(function) = expression else {
            unreachable!()
        };
        if !function.within_group.is_empty() {
            return Err(unsupported("ordered window arguments"));
        }
        let mut call = function.clone();
        let treatment = |value| match value {
            ast::NullTreatment::IgnoreNulls => NullTreatment::Ignore,
            ast::NullTreatment::RespectNulls => NullTreatment::Respect,
        };
        let mut null_treatment = function.null_treatment.map(treatment);
        if let ast::FunctionArguments::List(arguments) = &mut call.args {
            for clause in &arguments.clauses {
                if let ast::FunctionArgumentClause::IgnoreOrRespectNulls(treatment) = clause {
                    let value = match treatment {
                        ast::NullTreatment::IgnoreNulls => NullTreatment::Ignore,
                        ast::NullTreatment::RespectNulls => NullTreatment::Respect,
                    };
                    if null_treatment.replace(value).is_some() {
                        return Err(Error::Bind("duplicate window null treatment".into()));
                    }
                } else {
                    return Err(unsupported("window argument modifiers"));
                }
            }
            arguments.clauses.clear();
        }
        let options = WindowOptions {
            distinct: matches!(&function.args, ast::FunctionArguments::List(args) if args.duplicate_treatment == Some(ast::DuplicateTreatment::Distinct)),
            null_treatment,
            filtered: function.filter.is_some(),
        };
        let mut arguments = function_arguments(&call)?
            .iter()
            .map(|expr| self.expr(expr, fields, grouping))
            .collect::<Result<Vec<_>>>()?;
        let implementation = self.context.functions.window(&function.name.to_string())?;
        let types = implementation.argument_types(
            &arguments
                .iter()
                .map(|expr| expr.data_type.clone())
                .collect::<Vec<_>>(),
        )?;
        if types.len() != arguments.len() {
            return Err(Error::Internal(
                "window function changed argument cardinality".into(),
            ));
        }
        arguments = arguments
            .into_iter()
            .zip(&types)
            .map(|(argument, target)| {
                argument.cast(
                    target.clone(),
                    CastMode::Implicit,
                    self.context.casts,
                    self.context.query.types(),
                )
            })
            .collect::<Result<_>>()?;
        let data_type = implementation.return_type(&types, options, self.context.query.types())?;
        let partition = spec
            .partition_by
            .iter()
            .map(|expr| self.expr(expr, fields, grouping))
            .collect::<Result<Vec<_>>>()?;
        let order = spec
            .order_by
            .iter()
            .map(|key| {
                if key.with_fill.is_some() {
                    return Err(unsupported("window ORDER BY WITH FILL"));
                }
                let (descending, nulls_first) = self.context.query.settings().ordering(
                    key.options.asc,
                    key.options.nulls_first,
                    self.context.query,
                )?;
                Ok(OrderExpr {
                    expression: self.expr(&key.expr, fields, grouping)?,
                    descending,
                    nulls_first,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let filter = function
            .filter
            .as_ref()
            .map(|expr| {
                self.expr(expr, fields, grouping)
                    .and_then(|expr| self.boolean(expr))
            })
            .transpose()?;
        let spec_frame = spec.window_frame.clone().unwrap_or_default();
        let units = match spec_frame.units {
            ast::WindowFrameUnits::Rows => FrameUnits::Rows,
            ast::WindowFrameUnits::Range => FrameUnits::Range,
            ast::WindowFrameUnits::Groups => FrameUnits::Groups,
        };
        let bound = |bound: &ast::WindowFrameBound| -> Result<FrameBound> {
            Ok(match bound {
                ast::WindowFrameBound::CurrentRow => FrameBound::CurrentRow,
                ast::WindowFrameBound::Preceding(None) => FrameBound::UnboundedPreceding,
                ast::WindowFrameBound::Following(None) => FrameBound::UnboundedFollowing,
                ast::WindowFrameBound::Preceding(Some(expr)) => {
                    FrameBound::Preceding(self.nonnegative(expr)?)
                }
                ast::WindowFrameBound::Following(Some(expr)) => {
                    FrameBound::Following(self.nonnegative(expr)?)
                }
            })
        };
        let start = bound(&spec_frame.start_bound)?;
        let end = spec_frame
            .end_bound
            .as_ref()
            .map(bound)
            .transpose()?
            .unwrap_or(FrameBound::CurrentRow);
        let frame = WindowFrame { units, start, end };
        frame.validate()?;
        Ok(WindowExpression {
            function: implementation,
            arguments,
            partition,
            order,
            frame,
            options,
            filter,
            data_type,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Walk the supported scalar grammar without crossing a subquery's scope.
/// A callback returning false owns the subtree (aggregate/window binding).
pub(super) fn visit_expression<'a>(
    expression: &'a ast::Expr,
    visit: &mut impl FnMut(&'a ast::Expr) -> Result<bool>,
) -> Result<()> {
    if !visit(expression)? {
        return Ok(());
    }
    let mut child = |expr| visit_expression(expr, visit);
    match expression {
        ast::Expr::Function(function) => {
            if let ast::FunctionArguments::List(arguments) = &function.args {
                for argument in &arguments.args {
                    if let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expr)) = argument {
                        child(expr)?;
                    }
                }
            }
            if let Some(filter) = &function.filter {
                child(filter)?;
            }
            if let Some(ast::WindowType::WindowSpec(spec)) = &function.over {
                for expr in &spec.partition_by {
                    child(expr)?;
                }
                for key in &spec.order_by {
                    child(&key.expr)?;
                }
            }
        }
        ast::Expr::BinaryOp { left, right, .. } => {
            child(left)?;
            child(right)?;
        }
        ast::Expr::UnaryOp { expr, .. }
        | ast::Expr::Nested(expr)
        | ast::Expr::Cast { expr, .. }
        | ast::Expr::IsNull(expr)
        | ast::Expr::IsNotNull(expr) => child(expr)?,
        ast::Expr::Between {
            expr, low, high, ..
        } => {
            child(expr)?;
            child(low)?;
            child(high)?;
        }
        ast::Expr::InList { expr, list, .. } => {
            child(expr)?;
            for expr in list {
                child(expr)?;
            }
        }
        ast::Expr::InSubquery { expr, .. } => child(expr)?,
        ast::Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(expr) = operand {
                child(expr)?;
            }
            for condition in conditions {
                child(&condition.condition)?;
                child(&condition.result)?;
            }
            if let Some(expr) = else_result {
                child(expr)?;
            }
        }
        _ => (),
    }
    Ok(())
}
