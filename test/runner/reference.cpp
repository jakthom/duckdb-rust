// Independent persistent SQL oracle, linked only to the selected C++ DuckDB.
// Requests contain three UTF-8 byte-length-prefixed fields: operation,
// connection name, SQL. Responses are JSON lines, flushed per request.
#include "duckdb.hpp"
#include <algorithm>
#include <iostream>
#include <map>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string>

static std::string Quote(const std::string &value) {
    static constexpr char hex[] = "0123456789abcdef";
    std::string result = "\"";
    for (unsigned char c : value) {
        if (c == '\\' || c == '"') { result += '\\'; result += char(c); }
        else if (c < 32) {
            result += "\\u00"; result += hex[c >> 4]; result += hex[c & 15];
        } else result += char(c);
    }
    return result + '"';
}
static bool Read(std::string &value) {
    std::string length;
    if (!std::getline(std::cin, length)) return false;
    if (length.empty() || !std::all_of(length.begin(), length.end(), [](char c) { return c >= '0' && c <= '9'; }))
        throw std::runtime_error("invalid request length");
    auto size = std::stoull(length);
    if (size > 16 * 1024 * 1024) throw std::runtime_error("request field exceeds transport limit");
    value.resize(size);
    std::cin.read(value.data(), size);
    if (static_cast<size_t>(std::cin.gcount()) != size) throw std::runtime_error("incomplete request field");
    return true;
}
static std::string Cell(const duckdb::Value &value) {
    if (value.IsNull()) return "NULL";
    if (value.type().id() == duckdb::LogicalTypeId::BOOLEAN) return value.GetValue<bool>() ? "1" : "0";
    auto text = value.ToString();
    if (value.type().id() == duckdb::LogicalTypeId::VARCHAR) {
        if (text.empty()) return "(empty)";
        std::string escaped;
        for (char c : text) { if (c == '\0') escaped += "\\0"; else escaped += c; }
        return escaped;
    }
    return text;
}
// The release exposes public metadata; development uses accessors.
template <class RESULT>
static auto Types(RESULT &result, int) -> decltype(result.GetTypes()) { return result.GetTypes(); }
template <class RESULT>
static auto Types(RESULT &result, long) -> decltype((result.types)) { return result.types; }
int main() {
    try {
        auto database = std::make_unique<duckdb::DuckDB>(nullptr);
        std::map<std::string, std::unique_ptr<duckdb::Connection>> connections;
        std::cout << "{\"ready\":true,\"source_id\":" << Quote(duckdb::DuckDB::SourceID())
                  << ",\"version\":" << Quote(duckdb::DuckDB::LibraryVersion()) << "}" << std::endl;
        std::string operation, name, sql;
        while (Read(operation)) {
            if (!Read(name) || !Read(sql)) throw std::runtime_error("incomplete request");
            try {
                if (operation == "load" || operation == "reconnect") {
                    connections.clear();
                    if (operation == "load") database = std::make_unique<duckdb::DuckDB>(nullptr);
                    std::cout << "{\"ok\":true}" << std::endl;
                    continue;
                }
                if (operation != "query" && operation != "statement") throw std::runtime_error("unsupported transport operation");
                auto &connection = connections[name];
                if (!connection) connection = std::make_unique<duckdb::Connection>(*database);
                auto result = connection->Query(sql);
                duckdb::QueryResult *last = result.get();
                for (duckdb::QueryResult *current = result.get(); current; current = current->next.get()) {
                    if (current->HasError()) throw std::runtime_error(current->GetError());
                    last = current;
                }
                if (operation == "statement") {
                    std::cout << "{\"ok\":true}" << std::endl;
                    continue;
                }
                if (!last) throw std::runtime_error("query has no statements");
                std::ostringstream output;
                output << "{\"ok\":true,\"columns\":[";
                const auto &types = Types(*last, 0);
                for (size_t i = 0; i < types.size(); i++) {
                    if (i) output << ',';
                    output << Quote(types[i].ToString());
                }
                output << "],\"rows\":[";
                bool first_row = true;
                while (auto chunk = last->Fetch()) {
                    for (duckdb::idx_t row = 0; row < chunk->size(); row++) {
                        if (!first_row) output << ',';
                        first_row = false;
                        output << '[';
                        for (duckdb::idx_t column = 0; column < chunk->ColumnCount(); column++) {
                            if (column) output << ',';
                            output << Quote(Cell(chunk->GetValue(column, row)));
                        }
                        output << ']';
                    }
                }
                output << "]}";
                std::cout << output.str() << std::endl;
            } catch (const std::exception &error) {
                std::cout << "{\"ok\":false,\"message\":" << Quote(error.what()) << "}" << std::endl;
            }
        }
        return 0;
    } catch (const std::exception &error) {
        std::cerr << error.what() << std::endl;
        return 1;
    }
}
