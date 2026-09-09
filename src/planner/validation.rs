//! Shared invariants for SQL, alternative frontends, and optimizer output.
use crate::parallel::QueryContext;
use std::collections::HashSet;

use super::{
    BoundExpr, BoundStatement, ExprKind, LogicalPlan, PlanNode, Schema,
    expression::{BinaryOp, SubqueryKind, UnaryOp},
    logical::JoinKind,
};
use crate::{
    catalog::Catalog,
    common::{DataType, Error, Result},
};

struct ValidationScope<'a> {
    catalog: &'a dyn Catalog,
    query: &'a QueryContext,
    outer: &'a [Vec<DataType>],
}

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Bind(format!("invalid bound plan: {message}")))
    }
}
fn depth(depth: usize) -> Result<()> {
    if depth > 128 {
        Err(Error::Resource("bound plan nesting exceeds 128".into()))
    } else {
        Ok(())
    }
}
fn types(schema: &Schema) -> Vec<DataType> {
    schema.iter().map(|f| f.data_type.clone()).collect()
}
fn boolean(data_type: &DataType) -> Result<()> {
    require(*data_type == DataType::Boolean, "predicate must be BOOLEAN")
}

impl BoundStatement {
    pub fn validate(&self, catalog: &dyn Catalog, query: &QueryContext) -> Result<()> {
        self.validate_at(
            &ValidationScope {
                catalog,
                query,
                outer: &[],
            },
            0,
        )
    }
    fn validate_at(&self, scope: &ValidationScope<'_>, level: usize) -> Result<()> {
        depth(level)?;
        let (catalog, query) = (scope.catalog, scope.query);
        match self {
            Self::Query(plan) => plan.validate_at(scope, level + 1),
            Self::CreateTable {
                definition, source, ..
            } => {
                for column in &definition.columns {
                    query
                        .types()
                        .bind(&column.data_type)?
                        .validate(&column.default, query)?;
                }
                if let Some(source) = source {
                    source.validate_at(scope, level + 1)?;
                    require(
                        source.schema.len() == definition.columns.len(),
                        "CREATE source width",
                    )?;
                    require(
                        source
                            .schema
                            .iter()
                            .zip(&definition.columns)
                            .all(|(f, c)| f.data_type == c.data_type),
                        "CREATE assignment types",
                    )?;
                }
                Ok(())
            }
            Self::Insert {
                table,
                columns,
                source,
            } => {
                let definition = catalog.table(table)?;
                require(columns.len() == source.schema.len(), "INSERT source width")?;
                let mut seen = HashSet::new();
                for (column, field) in columns.iter().zip(&source.schema) {
                    require(
                        *column < definition.columns.len() && seen.insert(column),
                        "INSERT column identity",
                    )?;
                    require(
                        field.data_type == definition.columns[*column].data_type,
                        "INSERT assignment type",
                    )?;
                }
                source.validate_at(scope, level + 1)
            }
            Self::Update {
                table,
                assignments,
                predicate,
            } => {
                let input: Vec<_> = catalog
                    .table(table)?
                    .columns
                    .into_iter()
                    .map(|c| c.data_type)
                    .collect();
                let mut seen = HashSet::new();
                for (column, value) in assignments {
                    require(
                        *column < input.len() && seen.insert(column),
                        "UPDATE column identity",
                    )?;
                    value.validate_at(&input, level + 1, scope)?;
                    require(value.data_type == input[*column], "UPDATE assignment type")?;
                }
                if let Some(p) = predicate {
                    p.validate_at(&input, level + 1, scope)?;
                    boolean(&p.data_type)?;
                }
                Ok(())
            }
            Self::Delete { table, predicate } => {
                let input: Vec<_> = catalog
                    .table(table)?
                    .columns
                    .into_iter()
                    .map(|c| c.data_type)
                    .collect();
                if let Some(p) = predicate {
                    p.validate_at(&input, level + 1, scope)?;
                    boolean(&p.data_type)?;
                }
                Ok(())
            }
            Self::Explain(statement) => statement.validate_at(scope, level + 1),
            Self::CreateSchema { .. }
            | Self::DropSchema { .. }
            | Self::DropTable { .. }
            | Self::Begin
            | Self::Commit
            | Self::Rollback
            | Self::Checkpoint => Ok(()),
        }
    }
}

impl LogicalPlan {
    pub fn validate(&self, catalog: &dyn Catalog, query: &QueryContext) -> Result<()> {
        self.validate_at(
            &ValidationScope {
                catalog,
                query,
                outer: &[],
            },
            0,
        )
    }
    fn validate_at(&self, scope: &ValidationScope<'_>, level: usize) -> Result<()> {
        depth(level)?;
        let (catalog, query) = (scope.catalog, scope.query);
        let output = types(&self.schema);
        for data_type in &output {
            query.types().bind(data_type)?;
        }
        let expressions = |values: &[BoundExpr], input: &[DataType]| -> Result<Vec<DataType>> {
            for value in values {
                value.validate_at(input, level + 1, scope)?;
            }
            Ok(values.iter().map(|e| e.data_type.clone()).collect())
        };
        let expected = match &self.node {
            PlanNode::Values(rows) => {
                for row in rows {
                    require(expressions(row, &[])? == output, "VALUES types or width")?;
                }
                output.clone()
            }
            PlanNode::Scan(table) => catalog
                .table(table)?
                .columns
                .into_iter()
                .map(|c| c.data_type)
                .collect(),
            PlanNode::Range { step, .. } => {
                require(*step != 0, "zero range step")?;
                vec![DataType::BigInt]
            }
            PlanNode::KeyLookup {
                table,
                columns,
                key,
            } => {
                let table = catalog.table(table)?;
                require(
                    !columns.is_empty() && columns.len() == key.len(),
                    "lookup key width",
                )?;
                let mut seen = HashSet::new();
                for (&column, value) in columns.iter().zip(key) {
                    require(
                        column < table.columns.len() && seen.insert(column),
                        "lookup column identity",
                    )?;
                    let data_type = &table.columns[column].data_type;
                    require(value.fits_type(data_type), "lookup physical type")?;
                    query.types().bind(data_type)?.validate(value, query)?;
                }
                table.columns.into_iter().map(|c| c.data_type).collect()
            }
            PlanNode::Filter { input, predicate } => {
                input.validate_at(scope, level + 1)?;
                predicate.validate_at(&types(&input.schema), level + 1, scope)?;
                boolean(&predicate.data_type)?;
                types(&input.schema)
            }
            PlanNode::Projection {
                input,
                expressions: values,
            } => {
                input.validate_at(scope, level + 1)?;
                expressions(values, &types(&input.schema))?
            }
            PlanNode::Join {
                left,
                right,
                kind,
                condition,
            } => {
                left.validate_at(scope, level + 1)?;
                right.validate_at(scope, level + 1)?;
                let mut both = types(&left.schema);
                both.extend(types(&right.schema));
                condition.validate_at(&both, level + 1, scope)?;
                boolean(&condition.data_type)?;
                if matches!(kind, JoinKind::Semi | JoinKind::Anti) {
                    types(&left.schema)
                } else {
                    both
                }
            }
            PlanNode::Aggregate {
                input,
                groups,
                aggregates,
            } => {
                input.validate_at(scope, level + 1)?;
                let input = types(&input.schema);
                let mut output = expressions(groups, &input)?;
                for aggregate in aggregates {
                    let args = expressions(&aggregate.arguments, &input)?;
                    require(
                        aggregate.function.return_type(&args, query.types())?
                            == aggregate.data_type,
                        "aggregate result type",
                    )?;
                    if let Some(filter) = &aggregate.filter {
                        filter.validate_at(&input, level + 1, scope)?;
                        boolean(&filter.data_type)?;
                    }
                    output.push(aggregate.data_type.clone());
                }
                output
            }
            PlanNode::Sort { input, order } => {
                input.validate_at(scope, level + 1)?;
                for key in order {
                    key.expression
                        .validate_at(&types(&input.schema), level + 1, scope)?;
                }
                types(&input.schema)
            }
            PlanNode::Limit { input, .. } | PlanNode::Distinct(input) => {
                input.validate_at(scope, level + 1)?;
                types(&input.schema)
            }
            PlanNode::Union { left, right, .. } => {
                left.validate_at(scope, level + 1)?;
                right.validate_at(scope, level + 1)?;
                require(
                    types(&left.schema) == types(&right.schema),
                    "UNION input types",
                )?;
                types(&left.schema)
            }
        };
        require(
            expected == output,
            "operator schema disagrees with its semantics",
        )
    }
}

impl BoundExpr {
    fn validate_at(
        &self,
        input: &[DataType],
        level: usize,
        scope: &ValidationScope<'_>,
    ) -> Result<()> {
        depth(level)?;
        let query = scope.query;
        let bound_type = query.types().bind(&self.data_type)?;
        let visit = |expr: &BoundExpr| expr.validate_at(input, level + 1, scope);
        let expected = match &self.kind {
            ExprKind::OuterColumn { depth, column } => {
                let index = scope
                    .outer
                    .len()
                    .checked_sub(*depth)
                    .filter(|_| *depth > 0)
                    .ok_or_else(|| Error::Bind("correlated query depth outside scope".into()))?;
                scope.outer[index]
                    .get(*column)
                    .cloned()
                    .ok_or_else(|| Error::Bind("correlated column outside outer input".into()))?
            }
            ExprKind::Subquery(subquery) => {
                let crate::planner::expression::BoundSubquery { plan, kind } = subquery.as_ref();
                let mut outer = scope.outer.to_vec();
                outer.push(input.to_vec());
                plan.validate_at(
                    &ValidationScope {
                        outer: &outer,
                        ..*scope
                    },
                    level + 1,
                )?;
                match kind {
                    SubqueryKind::Exists { .. } => DataType::Boolean,
                    SubqueryKind::Scalar => {
                        require(plan.schema.len() == 1, "scalar subquery width")?;
                        plan.schema[0].data_type.clone()
                    }
                    SubqueryKind::In {
                        needle,
                        operand_type,
                        ..
                    } => {
                        visit(needle)?;
                        require(plan.schema.len() == 1, "IN subquery width")?;
                        require(
                            plan.schema[0].data_type == needle.data_type
                                && operand_type.data_type() == &needle.data_type,
                            "IN subquery comparison types",
                        )?;
                        DataType::Boolean
                    }
                }
            }
            ExprKind::Column(i) => input
                .get(*i)
                .cloned()
                .ok_or_else(|| Error::Bind("column outside input".into()))?,
            ExprKind::Literal(value) => {
                bound_type.validate(value, query)?;
                // Literals may carry a narrower explicit integer type.
                require(
                    value.fits_type(&self.data_type),
                    "literal physical type requires an explicit cast",
                )?;
                self.data_type.clone()
            }
            ExprKind::Cast(inner, cast, _) => {
                visit(inner)?;
                require(
                    cast.spec().source == inner.data_type && cast.spec().target == self.data_type,
                    "bound cast types",
                )?;
                self.data_type.clone()
            }
            ExprKind::Unary(op, inner) => {
                visit(inner)?;
                match op {
                    UnaryOp::Not => {
                        boolean(&inner.data_type)?;
                        DataType::Boolean
                    }
                    UnaryOp::IsNull | UnaryOp::IsNotNull => DataType::Boolean,
                }
            }
            ExprKind::Binary(op, left, right, operand_type) => {
                require(
                    operand_type.data_type() == &left.data_type,
                    "binary operand type adapter",
                )?;
                visit(left)?;
                visit(right)?;
                require(
                    left.data_type == right.data_type,
                    "binary operands need explicit coercions",
                )?;
                match op {
                    BinaryOp::And | BinaryOp::Or => {
                        boolean(&left.data_type)?;
                        DataType::Boolean
                    }
                    BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual => DataType::Boolean,
                }
            }
            ExprKind::Operator(function, args) => {
                let signature = function.signature();
                require(args.len() == signature.arguments.len(), "operator arity")?;
                for (argument, data_type) in args.iter().zip(&signature.arguments) {
                    visit(argument)?;
                    require(
                        argument.data_type == *data_type,
                        "operator operands require explicit coercion",
                    )?;
                }
                signature.result.clone()
            }
            ExprKind::Scalar(function, args) => {
                for arg in args {
                    visit(arg)?;
                }
                function.return_type(
                    &args.iter().map(|a| a.data_type.clone()).collect::<Vec<_>>(),
                    query.types(),
                )?
            }
            ExprKind::Case(branches, otherwise) => {
                visit(otherwise)?;
                for (condition, value) in branches {
                    visit(condition)?;
                    boolean(&condition.data_type)?;
                    visit(value)?;
                    require(value.data_type == otherwise.data_type, "CASE result types")?;
                }
                otherwise.data_type.clone()
            }
            ExprKind::InList(needle, values, _, operand_type) => {
                require(
                    operand_type.data_type() == &needle.data_type,
                    "IN operand type adapter",
                )?;
                visit(needle)?;
                for value in values {
                    visit(value)?;
                    require(value.data_type == needle.data_type, "IN operand types")?;
                }
                DataType::Boolean
            }
        };
        require(self.data_type == expected, "expression result type")
    }
}
