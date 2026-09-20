// One-shot two-connection D1 oracle linked solely against one pinned DuckDB.
#include "duckdb.hpp"
#include <iostream>
#include <string>

static void Exec(duckdb::Connection &connection, const std::string &sql) {
    auto result = connection.Query(sql); if (result->HasError()) throw std::runtime_error(result->GetError());
}
static int64_t Scalar(duckdb::Connection &connection, const std::string &sql) {
    auto result = connection.Query(sql); if (result->HasError()) throw std::runtime_error(result->GetError());
    auto chunk = result->Fetch(); if (!chunk || chunk->size()!=1) throw std::runtime_error("expected one scalar row");
    return chunk->GetValue(0,0).GetValue<int64_t>();
}
int main(int argc, char **argv) {
    try {
        if (argc != 2) throw std::runtime_error("expected D1 workload id"); std::string id(argv[1]);
        duckdb::DuckDB database(nullptr); duckdb::Connection left(database), right(database); int64_t rows=0, sum=0; int conflicts=0;
        if (id == "d1-disjoint-row-writers") { Exec(left,"CREATE TABLE t(i BIGINT PRIMARY KEY,v BIGINT); INSERT INTO t SELECT i,0 FROM range(10000) x(i)"); Exec(left,"BEGIN; UPDATE t SET v=1 WHERE i=1"); Exec(right,"BEGIN; UPDATE t SET v=2 WHERE i=2"); Exec(left,"COMMIT"); Exec(right,"COMMIT"); rows=Scalar(left,"SELECT count(*) FROM t"); sum=Scalar(left,"SELECT sum(v) FROM t"); }
        else if (id == "d1-contended-row-writers") { Exec(left,"CREATE TABLE t(i BIGINT PRIMARY KEY,v BIGINT); INSERT INTO t VALUES (1,0)"); Exec(left,"BEGIN; UPDATE t SET v=1 WHERE i=1"); Exec(right,"BEGIN; DELETE FROM t WHERE i=1"); Exec(left,"COMMIT"); auto result=right.Query("COMMIT"); if (!result->HasError()) throw std::runtime_error("contended workload accepted two winners"); rows=Scalar(left,"SELECT count(*) FROM t"); sum=Scalar(left,"SELECT sum(v) FROM t"); conflicts=1; }
        else if (id == "d1-catalog-disjoint-and-contended") { Exec(left,"BEGIN; CREATE TABLE a(i BIGINT)"); Exec(right,"BEGIN; CREATE TABLE b(i BIGINT)"); Exec(left,"COMMIT"); Exec(right,"COMMIT"); Exec(left,"BEGIN; CREATE TABLE same(i BIGINT)"); Exec(right,"BEGIN; CREATE TABLE same(i BIGINT)"); Exec(left,"COMMIT"); auto result=right.Query("COMMIT"); if (!result->HasError()) throw std::runtime_error("same catalog object accepted two winners"); rows=Scalar(left,"SELECT count(*) FROM information_schema.tables WHERE table_name IN ('a','b','same')"); conflicts=1; }
        else if (id == "d1-retained-reader-publication") { Exec(left,"CREATE TABLE t(i BIGINT); INSERT INTO t SELECT i FROM range(10000) x(i)"); Exec(right,"BEGIN"); Exec(left,"INSERT INTO t VALUES (10000)"); auto old=Scalar(right,"SELECT count(*) FROM t"); Exec(right,"COMMIT"); auto fresh=Scalar(left,"SELECT count(*) FROM t"); if(old!=10000 || fresh!=10001) throw std::runtime_error("retained reader visibility differs"); rows=fresh; sum=old+fresh; }
        else throw std::runtime_error("unknown D1 workload");
        std::cout << "{\"schema\":\"d1-worker-v1\",\"engine\":\"cpp\",\"source_id\":\"" << duckdb::DuckDB::SourceID() << "\",\"id\":\"" << id << "\",\"rows\":" << rows << ",\"checksum\":\"" << sum << "\",\"conflicts\":" << conflicts << "}" << std::endl;
    } catch(const std::exception &error) { std::cerr << error.what() << std::endl; return 1; }
}
