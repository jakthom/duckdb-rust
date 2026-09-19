use super::*;
use duckdb_rust::common::{
    NestedPayload,
    cast::{BoundCast, CastFunction, CastMode, CastRegistry, CastSpec},
};

#[derive(Debug)]
struct TypedSource;

#[derive(Debug)]
struct TypedBind {
    cast: BoundCast,
    input: Value,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableFunction for TypedSource {
    fn name(&self) -> &str {
        "typed_source"
    }

    fn bind(
        &self,
        args: &[TableFunctionArgument],
        context: &TableFunctionBindContext<'_>,
    ) -> Result<TableFunctionBind> {
        let [argument] = args else {
            return Err(Error::Bind("one STRUCT argument required".into()));
        };
        let Value::Nested(value) = &argument.value else {
            return Err(Error::Bind("STRUCT required".into()));
        };
        let NestedPayload::Struct(fields) = &value.payload else {
            return Err(Error::Bind("STRUCT required".into()));
        };
        let [Value::Varchar(type_name), input @ Value::Varchar(_)] = fields.as_slice() else {
            return Err(Error::Bind("type and input strings required".into()));
        };
        let target = (context.resolve_type)(type_name)?;
        let cast = context.casts.bind(
            &DataType::Varchar,
            &target,
            CastMode::Explicit,
            context.query.types(),
        )?;
        Ok(TableFunctionBind::new(
            vec![Field::new("value", target)],
            TypedBind {
                cast,
                input: input.clone(),
            },
        ))
    }

    fn init(&self, _: &TableFunctionBind, _: &QueryContext) -> Result<Box<dyn TableFunctionState>> {
        Ok(Box::new(false))
    }

    fn scan(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<DataChunk>> {
        let done = state.downcast_mut::<bool>().unwrap();
        if *done || max_rows == 0 {
            return Ok(None);
        }
        *done = true;
        let data = bind.data().downcast_ref::<TypedBind>().unwrap();
        let value = data.cast.apply(&data.input, context)?;
        DataChunk::from_rows(&[bind.schema()[0].data_type.clone()], &[vec![value]]).map(Some)
    }
}

#[derive(Debug)]
struct SelectedInteger;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedInteger {
    fn name(&self) -> &'static str {
        "selected-test-integer"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar && spec.target == DataType::Integer
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        Ok(Value::Integer(42))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn table_binding_uses_nested_constants_selected_casts_and_catalog_types() -> Result<()> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_table(Arc::new(TypedSource))?;
    let mut casts = CastRegistry::builtins();
    casts.replace(
        CastSpec {
            source: DataType::Varchar,
            target: DataType::Integer,
            mode: CastMode::Explicit,
        },
        Arc::new(SelectedInteger),
    )?;
    let mut connection = DatabaseBuilder::new()
        .functions(functions)
        .casts(casts)
        .build()?
        .connect();
    assert_eq!(
        connection
            .query("SELECT * FROM typed_source({'type':'INT','input':'not an integer'})")?
            .rows,
        ints(&[42])
    );
    assert_eq!(
        connection
            .query("SELECT * FROM typed_source({'type':'DECIMAL(6,2)','input':'1.25'})")?
            .rows,
        vec![vec![Value::Decimal {
            value: 125,
            width: 6,
            scale: 2
        }]]
    );
    connection.execute("CREATE SCHEMA s")?;
    connection.execute("CREATE TYPE s.mood AS ENUM ('sad','ok')")?;
    connection.execute("SET search_path='s'")?;
    assert_eq!(
        connection
            .query("SELECT value::VARCHAR FROM typed_source({'type':'mood','input':'ok'})")?
            .rows,
        vec![vec![Value::Varchar("ok".into())]]
    );
    for type_name in [
        "",
        "INTEGER; SELECT 1",
        "INTEGER trailing",
        "DECIMAL(",
        "main.missing",
    ] {
        let sql = format!("SELECT * FROM typed_source({{'type':'{type_name}','input':'1'}})");
        assert!(connection.query(&sql).is_err(), "{type_name}");
    }
    assert_eq!(
        connection.query("SELECT * FROM range(2)")?.rows,
        ints(&[0, 1])
    );
    Ok(())
}
