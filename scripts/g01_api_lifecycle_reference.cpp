#include "duckdb.hpp"
#include <iostream>
#include <stdexcept>
using namespace duckdb;
static void ok(unique_ptr<QueryResult> result) { if (result->HasError()) throw std::runtime_error(result->GetError()); }
static int value(Connection &connection, const std::string &sql) { auto result = connection.Query(sql); if (result->HasError()) throw std::runtime_error(result->GetError()); return result->GetValue(0, 0).GetValue<int>(); }
int main() {
 try {
  DuckDB db(nullptr); Connection survivor(db); ok(survivor.Query("CREATE TABLE t(i INTEGER);")); ok(survivor.Query("INSERT INTO t VALUES (1);"));
  { Connection short_lived(db); ok(short_lived.Query("BEGIN; UPDATE t SET i=2; CREATE TABLE rolled_back(i INTEGER);")); }
  if (value(survivor, "SELECT i FROM t") != 1) throw std::runtime_error("dropped transaction committed");
  auto missing = survivor.Query("SELECT * FROM rolled_back"); if (!missing->HasError()) throw std::runtime_error("dropped transaction retained DDL");
  ok(survivor.Query("CREATE TABLE after_drop(i INTEGER); INSERT INTO after_drop VALUES (7);"));
  if (value(survivor, "SELECT i FROM after_drop") != 7) throw std::runtime_error("surviving connection unusable");
  std::cout << "G01_API_LIFECYCLE_PASS 3\n";
 } catch (std::exception &error) { std::cerr << error.what() << "\n"; return 1; }
}
