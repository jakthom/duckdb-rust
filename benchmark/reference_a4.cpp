// Independent full-outer-join classifier for already-validated A4 CSV relations.
#include "duckdb.hpp"
#include <iostream>
#include <string>

int main(int argc, char **argv) {
    try {
        if (argc != 3) throw std::runtime_error("expected baseline and current CSV paths");
        duckdb::DuckDB db(nullptr); duckdb::Connection con(db);
        std::cout << "READY\t" << duckdb::DuckDB::SourceID() << "\t" << duckdb::DuckDB::LibraryVersion() << "\n";
        auto setup = con.Query("CREATE TABLE baseline AS SELECT * FROM read_csv_auto('" + std::string(argv[1]) + "', header=true);"
                               "CREATE TABLE current AS SELECT * FROM read_csv_auto('" + std::string(argv[2]) + "', header=true);");
        if (setup->HasError()) throw std::runtime_error(setup->GetError());
        auto result = con.Query("SELECT coalesce(b.case_path,c.case_path), coalesce(b.pin,c.pin), coalesce(b.runtime_config_digest,c.runtime_config_digest), "
          "CASE WHEN b.case_path IS NULL THEN 'new_current_only' WHEN c.case_path IS NULL AND b.status='passed' THEN 'lost_pass_omitted' "
          "WHEN c.case_path IS NULL THEN 'baseline_only_omitted' WHEN b.configuration_changed OR c.configuration_changed THEN 'uncomparable_configuration' WHEN b.is_stale OR c.is_stale THEN 'stale' "
          "WHEN b.population_digest<>c.population_digest THEN 'uncomparable_population' WHEN b.status='passed' AND c.status<>'passed' THEN 'fresh_failure' "
          "WHEN b.status=c.status THEN 'unchanged_' || c.status ELSE 'changed_nonpass' END FROM baseline b FULL OUTER JOIN current c "
          "ON b.case_path=c.case_path AND b.pin=c.pin AND b.runtime_config_digest=c.runtime_config_digest ORDER BY 1,2,3");
        if (result->HasError()) throw std::runtime_error(result->GetError());
        while (auto chunk = result->Fetch()) for (duckdb::idx_t i=0; i<chunk->size(); ++i)
            std::cout << chunk->GetValue(0,i).ToString() << "\t" << chunk->GetValue(1,i).ToString() << "\t" << chunk->GetValue(2,i).ToString() << "\t" << chunk->GetValue(3,i).ToString() << "\n";
    } catch (const std::exception &error) { std::cerr << error.what() << std::endl; return 1; }
}
