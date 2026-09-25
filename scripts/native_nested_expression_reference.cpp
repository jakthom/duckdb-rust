// Raw parsed-expression evidence: no binding, evaluation or catalog mutation.
// Link only the already-built pinned C++ library in a worker-local helper.
#include "duckdb.hpp"
#include "duckdb/parser/parser.hpp"
#include "duckdb/parser/parsed_expression_iterator.hpp"
#include "duckdb/parser/expression/constant_expression.hpp"
#include "duckdb/parser/expression/columnref_expression.hpp"
#include "duckdb/parser/expression/function_expression.hpp"
#include "duckdb/common/serializer/binary_serializer.hpp"
#include "duckdb/common/serializer/binary_deserializer.hpp"
#include "duckdb/common/serializer/memory_stream.hpp"
#include <iostream>
#include <iterator>
#include <stdexcept>
#include <string>
#include <vector>

static std::string hex(const void *data, size_t length) {
    auto bytes = static_cast<const uint8_t *>(data);
    static const char *digits = "0123456789abcdef";
    std::string output;
    for (size_t i = 0; i < length; i++) {
        output += digits[bytes[i] >> 4];
        output += digits[bytes[i] & 15];
    }
    return output;
}
static std::string hex(const std::string &value) { return hex(value.data(), value.size()); }
static unsigned digit(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    throw std::runtime_error("invalid hexadecimal input");
}
static std::string alias(const duckdb::ParsedExpression &expression) {
#ifdef NESTED_EXPRESSION_RELEASE
    return expression.GetAlias();
#else
    return expression.GetAlias().GetIdentifierName();
#endif
}
static void inventory(const duckdb::ParsedExpression &expression, size_t depth, size_t &nodes) {
    if (depth > 64 || ++nodes > 16384) throw std::runtime_error("expression inventory limit");
    std::cout << "node " << depth << " " << int(expression.GetExpressionClass()) << " "
              << int(expression.GetExpressionType()) << " " << hex(alias(expression)) << "\n";
    if (expression.GetExpressionClass() == duckdb::ExpressionClass::CONSTANT) {
#ifdef NESTED_EXPRESSION_RELEASE
        auto &value = expression.Cast<duckdb::ConstantExpression>().value;
#else
        auto &value = expression.Cast<duckdb::ConstantExpression>().GetValue();
#endif
        std::cout << "literal " << hex(value.type().ToString()) << " " << value.IsNull() << "\n";
    }
    if (expression.GetExpressionClass() == duckdb::ExpressionClass::COLUMN_REF) {
        auto &column = expression.Cast<duckdb::ColumnRefExpression>();
        std::cout << "column";
#ifdef NESTED_EXPRESSION_RELEASE
        for (auto &part : column.column_names) std::cout << " " << hex(part);
#else
        for (auto &part : column.ColumnNames()) std::cout << " " << hex(part.GetIdentifierName());
#endif
        std::cout << "\n";
    }
    if (expression.GetExpressionClass() == duckdb::ExpressionClass::FUNCTION) {
        auto &function = expression.Cast<duckdb::FunctionExpression>();
#ifdef NESTED_EXPRESSION_RELEASE
        std::cout << "function " << function.is_operator << " legacy " << hex(function.catalog) << " "
                  << hex(function.schema) << " " << hex(function.function_name) << "\n";
        for (auto &child : function.children) std::cout << "argument " << hex(alias(*child)) << "\n";
#else
        std::cout << "function " << function.IsOperator() << " "
                  << (function.IsLegacyFunctionCall() ? "legacy" : "named");
        for (auto &part : function.GetQualifiedName().Path()) std::cout << " " << hex(part.GetIdentifierName());
        std::cout << "\n";
        for (auto &argument : function.GetArguments()) std::cout << "argument " << hex(argument.GetName().GetIdentifierName()) << "\n";
#endif
    }
    duckdb::ParsedExpressionIterator::EnumerateChildren(expression, [&](const duckdb::ParsedExpression &child) {
        inventory(child, depth + 1, nodes);
    });
}
int main(int argc, char **argv) {
    try {
        if (argc != 3) throw std::runtime_error("usage: helper parse|decode|identity version (stdin payload)");
        if (std::string(argv[1]) == "identity") {
            std::cout << duckdb::DuckDB::LibraryVersion() << "\n" << duckdb::DuckDB::SourceID() << "\n";
            return 0;
        }
        std::string input((std::istreambuf_iterator<char>(std::cin)), std::istreambuf_iterator<char>());
        duckdb::unique_ptr<duckdb::ParsedExpression> expression;
        if (std::string(argv[1]) == "parse") {
            auto parsed = duckdb::Parser::ParseExpressionList(input);
            if (parsed.size() != 1) throw std::runtime_error("exactly one parsed expression required");
            expression = std::move(parsed[0]);
        } else if (std::string(argv[1]) == "decode") {
            if (input.size() % 2) throw std::runtime_error("odd hexadecimal input");
            std::vector<uint8_t> bytes;
            for (size_t i = 0; i < input.size(); i += 2) bytes.push_back(digit(input[i]) * 16 + digit(input[i + 1]));
            duckdb::MemoryStream stream(bytes.data(), bytes.size());
            duckdb::BinaryDeserializer deserializer(stream);
            deserializer.Begin();
            expression = duckdb::ParsedExpression::Deserialize(deserializer);
            deserializer.End();
            if (stream.GetPosition() != bytes.size()) throw std::runtime_error("trailing parsed-expression input");
        } else throw std::runtime_error("unknown mode");
        duckdb::SerializationOptions options;
#ifdef NESTED_EXPRESSION_RELEASE
        options.serialization_compatibility = duckdb::SerializationCompatibility::FromString(argv[2]);
#else
        options.storage_compatibility = duckdb::StorageCompatibility::FromString(argv[2]);
#endif
        duckdb::MemoryStream output;
        duckdb::BinarySerializer::Serialize(*expression, output, options);
        std::cout << hex(output.GetData(), output.GetPosition()) << "\n";
        // Diagnostic display is not the equality oracle. Retain wire and raw
        // structural/name metadata independently, including unbound targets.
        std::cout << hex(expression->ToString()) << "\n";
        size_t nodes = 0;
        inventory(*expression, 0, nodes);
        return 0;
    } catch (std::exception &error) {
        std::cerr << error.what() << "\n";
        return 1;
    }
}
