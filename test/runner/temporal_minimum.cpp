// Independently construct physical timestamp minima through pinned C++ Value
// and Appender APIs, without relying on SQL text that cannot render those values.
#include "duckdb.hpp"
#include <filesystem>
#include <iostream>
#include <limits>
#include <stdexcept>
#include <vector>

using namespace duckdb;

static void Execute(Connection &connection, const std::string &sql) {
    auto result = connection.Query(sql);
    if (result->HasError()) throw std::runtime_error(result->GetError());
}

static std::vector<Value> Timestamps(int64_t ticks) {
    std::vector<Value> values {
        Value::TIMESTAMP(timestamp_t(ticks)), Value::TIMESTAMPSEC(timestamp_sec_t(ticks)),
        Value::TIMESTAMPMS(timestamp_ms_t(ticks)), Value::TIMESTAMPNS(timestamp_ns_t(ticks)),
        Value::TIMESTAMPTZ(timestamp_tz_t(ticks))
    };
#ifdef DDB_DEVELOPMENT
    values.push_back(Value::TIMESTAMPTZNS(timestamp_tz_ns_t(ticks)));
#endif
    return values;
}

static int64_t Ticks(const Value &value, idx_t column) {
    switch (column) {
    case 0: return value.GetValueUnsafe<timestamp_t>().value;
    case 1: return value.GetValueUnsafe<timestamp_sec_t>().value;
    case 2: return value.GetValueUnsafe<timestamp_ms_t>().value;
    case 3: return value.GetValueUnsafe<timestamp_ns_t>().value;
    case 4: return value.GetValueUnsafe<timestamp_tz_t>().value;
#ifdef DDB_DEVELOPMENT
    case 5: return value.GetValueUnsafe<timestamp_tz_ns_t>().value;
#endif
    default: throw std::runtime_error("timestamp fixture column");
    }
}

static void Inspect(const std::string &path, bool mutated) {
    DBConfig config;
    config.options.access_mode = AccessMode::READ_ONLY;
    DuckDB reopened(path, &config);
    Connection connection(reopened);
    auto result = connection.Query("SELECT * FROM minima ORDER BY id");
    if (result->HasError()) throw std::runtime_error(result->GetError());
    if (result->RowCount() != 6 || result->ColumnCount() != Timestamps(0).size()+1) {
        throw std::runtime_error("C++ fixture changed shape");
    }
    const auto minimum = std::numeric_limits<int64_t>::min();
    const auto typed_columns = Timestamps(0);
    idx_t checked = 0;
    for (idx_t row = 0; row < 6; ++row) {
        if (result->GetValue(0, row).GetValue<int32_t>() != int32_t(row)) throw std::runtime_error("C++ fixture changed row identity");
        for (idx_t column = 1; column < result->ColumnCount(); ++column) {
            const auto value = result->GetValue(column, row);
            if (value.type() != typed_columns[column - 1].type()) throw std::runtime_error("C++ fixture changed timestamp logical type");
            if (value.IsNull() != (row == 4)) throw std::runtime_error("C++ fixture lost MIN versus NULL");
            if (!value.IsNull()) {
                const auto expected = row == 0 || row == 5 || (mutated && row == 3) ? minimum : row == 1 ? -std::numeric_limits<int64_t>::max() : row == 2 ? minimum + 2 : 0;
                if (Ticks(value, column - 1) != expected) throw std::runtime_error("C++ fixture changed timestamp ticks");
            }
            ++checked;
        }
    }
    std::cout << "{\"checked_values\":" << checked << ",\"passed\":true}\n";
}

int main(int argc, char **argv) {
    try {
        if (argc == 4 && std::string(argv[1]) == "--inspect") {
            if (std::string(argv[3]) != "original" && std::string(argv[3]) != "mutated") throw std::runtime_error("invalid inspection stage");
            Inspect(argv[2], std::string(argv[3]) == "mutated");
            return 0;
        }
        if (argc != 3) throw std::runtime_error("database path and copied WAL prefix required");
        const std::filesystem::path path(argv[1]), copy(argv[2]);
        if (std::filesystem::exists(path) || std::filesystem::exists(copy)) throw std::runtime_error("refuse existing fixture");
        const int64_t minimum = std::numeric_limits<int64_t>::min();
        {
            DuckDB database(path.string());
            Connection connection(database);
            Execute(connection, "PRAGMA disable_optimizer");
            std::string columns = "u TIMESTAMP,s TIMESTAMP_S,ms TIMESTAMP_MS,n TIMESTAMP_NS,z TIMESTAMPTZ";
#ifdef DDB_DEVELOPMENT
            columns += ",zn TIMESTAMPTZ_NS";
#endif
            Execute(connection, "CREATE TABLE minima(id INTEGER PRIMARY KEY," + columns + ")");
            Execute(connection, "CHECKPOINT");
            {
                Appender appender(connection, "minima");
                for (int32_t row = 0; row < 6; ++row) {
                    appender.BeginRow();
                    appender.Append(row);
                    for (const auto &value : Timestamps(row == 0 || row == 5 ? minimum : row == 1 ? -std::numeric_limits<int64_t>::max() : row == 2 ? minimum + 2 : 0)) {
                        appender.Append(row == 4 ? Value(value.type()) : value);
                    }
                    appender.EndRow();
                }
                appender.Close();
            }
            // Copy while the writer remains alive: this preserves independently
            // generated committed WAL before the destructor's checkpoint.
            std::filesystem::copy_file(path, copy);
            std::filesystem::copy_file(path.string()+".wal", copy.string()+".wal");
            Execute(connection, "CHECKPOINT");
        }
        Inspect(path.string(), false);
    } catch (const std::exception &error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
