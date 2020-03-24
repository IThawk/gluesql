use crate::data::Row;
use crate::executor::{
    fetch_columns, Blend, BlendContext, BlendedFilter, Filter, FilterContext, Limit,
};
use crate::storage::Store;
use nom_sql::{
    Column, JoinClause, JoinConstraint, JoinOperator, JoinRightSide, SelectStatement, Table,
};
use std::fmt::Debug;

pub struct SelectParams<'a> {
    pub table: &'a Table,
    pub columns: Vec<Column>,
    pub join_columns: Vec<(&'a Table, Vec<Column>)>,
}

pub fn fetch_select_params<'a, T: 'static + Debug>(
    storage: &'a dyn Store<T>,
    statement: &'a SelectStatement,
) -> SelectParams<'a> {
    let SelectStatement {
        tables,
        join: join_clauses,
        ..
    } = statement;
    let table = tables
        .iter()
        .next()
        .expect("SelectStatement->tables should have something");

    let columns = fetch_columns(storage, table);
    let join_columns = join_clauses
        .iter()
        .map(|JoinClause { right, .. }| {
            let table = match &right {
                JoinRightSide::Table(table) => table,
                _ => unimplemented!(),
            };

            (table, fetch_columns(storage, table))
        })
        .collect();

    SelectParams {
        table,
        columns,
        join_columns,
    }
}

fn fetch_blended<'a, T: 'static + Debug>(
    storage: &dyn Store<T>,
    table: &'a Table,
    columns: &'a Vec<Column>,
) -> Box<dyn Iterator<Item = BlendContext<'a, T>> + 'a> {
    let rows = storage
        .get_data(&table.name)
        .unwrap()
        .map(move |(key, row)| (columns, key, row))
        .map(move |(columns, key, row)| BlendContext {
            table,
            columns,
            key,
            row,
            next: None,
        });

    Box::new(rows)
}

fn join<'a, T: 'static + Debug>(
    storage: &'a dyn Store<T>,
    join_clause: &'a JoinClause,
    table: &'a Table,
    columns: &'a Vec<Column>,
    filter_context: Option<&'a FilterContext<'a>>,
    blend_context: BlendContext<'a, T>,
) -> Option<BlendContext<'a, T>> {
    let JoinClause {
        operator,
        constraint,
        ..
    } = join_clause;
    let where_clause = match constraint {
        JoinConstraint::On(where_clause) => Some(where_clause),
        _ => unimplemented!(),
    };
    let filter = Filter::new(storage, where_clause, filter_context);
    let blended_filter = BlendedFilter::new(&filter, &blend_context);

    let row = storage
        .get_data(&table.name)
        .unwrap()
        .map(move |(key, row)| (columns, key, row))
        .filter(move |(columns, _, row)| blended_filter.check(Some((table, columns, row))))
        .next();

    match row {
        Some((columns, key, row)) => Some(BlendContext {
            table,
            columns,
            key,
            row,
            next: Some(Box::new(blend_context)),
        }),
        None => match operator {
            JoinOperator::LeftJoin | JoinOperator::LeftOuterJoin => Some(blend_context),
            JoinOperator::Join | JoinOperator::InnerJoin => None,
            _ => unimplemented!(),
        },
    }
}

pub fn select<'a, T: 'static + Debug>(
    storage: &'a dyn Store<T>,
    statement: &'a SelectStatement,
    params: &'a SelectParams<'a>,
    filter_context: Option<&'a FilterContext<'a>>,
) -> Box<dyn Iterator<Item = Row> + 'a> {
    let SelectStatement {
        where_clause,
        limit: limit_clause,
        join: join_clauses,
        fields,
        ..
    } = statement;
    let SelectParams {
        table,
        columns,
        join_columns,
    } = params;

    let blend = Blend::new(fields);
    let filter = Filter::new(storage, where_clause.as_ref(), filter_context);
    let limit = Limit::new(limit_clause);

    let rows = fetch_blended(storage, table, columns)
        .filter_map(move |init_context| {
            join_clauses.iter().zip(join_columns.iter()).fold(
                Some(init_context),
                |blend_context, (join_clause, (table, columns))| {
                    blend_context.and_then(|blend_context| {
                        join(
                            storage,
                            join_clause,
                            table,
                            columns,
                            filter_context,
                            blend_context,
                        )
                    })
                },
            )
        })
        .filter(move |blend_context| BlendedFilter::new(&filter, &blend_context).check(None))
        .enumerate()
        .filter_map(move |(i, item)| match limit.check(i) {
            true => Some(item),
            false => None,
        })
        .map(move |BlendContext { columns, row, .. }| blend.apply(&columns, row));

    Box::new(rows)
}
