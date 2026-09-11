// Independent measurement worker linked only to the pinned C++ DuckDB engine.
#include "duckdb.hpp"
#include <chrono>
#include <fstream>
#include <iostream>
#include <sstream>

static std::string Read(const char *path) {
    std::ifstream input(path);
    if (!input) throw std::runtime_error("cannot read workload");
    std::ostringstream text;
    text << input.rdbuf();
    return text.str();
}

int main(int argc, char **argv) {
    try {
        if (argc != 3 && argc != 5) throw std::runtime_error("expected setup/query and optional reset/verification paths");
        duckdb::DuckDB database(nullptr);
        duckdb::Connection connection(database);
        auto configuration = connection.Query("SET threads=1");
        if (configuration->HasError()) throw std::runtime_error(configuration->GetError());
        auto setup = connection.Query(Read(argv[1]));
        if (setup->HasError()) throw std::runtime_error(setup->GetError());
        std::string reset_sql, verification_sql;
        if (argc == 5) {
            reset_sql = Read(argv[3]);
            verification_sql = Read(argv[4]);
            auto reset = connection.Query(reset_sql);
            if (reset->HasError()) throw std::runtime_error(reset->GetError());
        }
        auto statement = connection.Prepare(Read(argv[2]));
        if (statement->HasError()) throw std::runtime_error(statement->GetError());
        std::cout << "{\"ready\":true,\"engine\":\"cpp\",\"threads\":1,\"source_id\":\""
                  << duckdb::DuckDB::SourceID() << "\",\"version\":\""
                  << duckdb::DuckDB::LibraryVersion() << "\"}" << std::endl;
        std::string request;
        while (std::getline(std::cin, request)) {
            if (request != "sample") throw std::runtime_error("expected sample request");
            if (argc == 5) {
                auto reset = connection.Query(reset_sql);
                if (reset->HasError()) throw std::runtime_error(reset->GetError());
            }
            auto start = std::chrono::steady_clock::now();
            duckdb::vector<duckdb::Value> parameters;
            auto result = statement->Execute(parameters, false);
            if (result->HasError()) throw std::runtime_error(result->GetError());
            int64_t sum = 0;
            uint64_t rows = 0;
            while (auto chunk = result->Fetch()) {
                rows += chunk->size();
                for (duckdb::idx_t row = 0; row < chunk->size(); row++) {
                    for (duckdb::idx_t column = 0; column < chunk->ColumnCount(); column++) {
                        auto value = chunk->GetValue(column, row);
                        if (value.IsNull()) throw std::runtime_error("unexpected NULL checksum");
                        int64_t next;
                        if (__builtin_add_overflow(sum, value.GetValue<int64_t>(), &next))
                            throw std::runtime_error("checksum overflow");
                        sum = next;
                    }
                }
            }
            auto elapsed = std::chrono::duration_cast<std::chrono::nanoseconds>(
                std::chrono::steady_clock::now() - start).count();
            if (argc == 5) {
                if (rows != 0) throw std::runtime_error("DDL measurement unexpectedly returned rows");
                auto verification = connection.Query(verification_sql);
                if (verification->HasError()) throw std::runtime_error(verification->GetError());
                while (auto chunk = verification->Fetch()) {
                    rows += chunk->size();
                    for (duckdb::idx_t row = 0; row < chunk->size(); row++) {
                        for (duckdb::idx_t column = 0; column < chunk->ColumnCount(); column++) {
                            auto value = chunk->GetValue(column, row);
                            if (value.IsNull()) throw std::runtime_error("unexpected NULL verification");
                            int64_t next;
                            if (__builtin_add_overflow(sum, value.GetValue<int64_t>(), &next))
                                throw std::runtime_error("verification overflow");
                            sum = next;
                        }
                    }
                }
            }
            std::cout << "{\"elapsed_ns\":" << elapsed << ",\"rows\":" << rows
                      << ",\"sum\":\"" << sum << "\"}" << std::endl;
        }
        return 0;
    } catch (const std::exception &error) {
        std::cerr << error.what() << std::endl;
        return 1;
    }
}
