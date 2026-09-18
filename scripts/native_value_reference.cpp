// Independent raw Value metadata oracle. Link the existing pinned library;
// never rebuild or modify the C++ reference checkout for this helper.
#include "duckdb.hpp"
#include "duckdb/common/serializer/binary_serializer.hpp"
#include "duckdb/common/serializer/binary_deserializer.hpp"
#include "duckdb/common/serializer/memory_stream.hpp"
#include "duckdb/function/scalar/variant_utils.hpp"
#include <iostream>
#include <iterator>
#include <stdexcept>
#include <string>
#include <vector>

static std::string hex(const uint8_t *data, size_t size) {
    static const char *digits = "0123456789abcdef";
    std::string output;
    for (size_t i = 0; i < size; i++) {
        output.push_back(digits[data[i] >> 4]);
        output.push_back(digits[data[i] & 15]);
    }
    return output;
}
static unsigned digit(char byte) {
    if (byte >= '0' && byte <= '9') return byte - '0';
    if (byte >= 'a' && byte <= 'f') return byte - 'a' + 10;
    throw std::runtime_error("invalid hexadecimal input");
}

// Canonical *content* evidence is distinct from descriptor allocation order,
// unused payload bytes and dictionary slots. Use C++'s own tagged VARIANT
// traversal, then its scalar Value serializer to retain widths and raw bits.
static void append(std::string &output, const void *data, size_t count) {
    uint64_t length = count;
    output.append(reinterpret_cast<const char *>(&length), sizeof(length));
    output.append(reinterpret_cast<const char *>(data), count);
}
static void visit(size_t depth, size_t &remaining) {
    if (depth > 64 || !remaining) throw std::runtime_error("content traversal limit");
    remaining--;
}
static void content(const duckdb::Value &, std::string &, const duckdb::SerializationOptions &, size_t, size_t &);
static void variant_content(const duckdb::UnifiedVariantVectorData &variant, uint32_t index,
                            std::string &output, const duckdb::SerializationOptions &options,
                            size_t depth, size_t &remaining) {
    visit(depth, remaining);
    auto tag = variant.GetTypeId(0, index);
    output.push_back(static_cast<char>(tag));
    if (tag == duckdb::VariantLogicalType::VARIANT_NULL) return;
    if (tag == duckdb::VariantLogicalType::ARRAY || tag == duckdb::VariantLogicalType::OBJECT) {
        auto children = duckdb::VariantUtils::DecodeNestedData(variant, 0, index);
        uint64_t count = children.child_count;
        append(output, &count, sizeof(count));
        for (uint32_t i = 0; i < children.child_count; i++) {
            auto child = children.children_idx + i;
            if (tag == duckdb::VariantLogicalType::OBJECT) {
                auto &key = variant.GetKey(0, variant.GetKeysIndex(0, child));
                append(output, key.GetData(), key.GetSize());
            }
            variant_content(variant, variant.GetValuesIndex(0, child), output, options, depth + 1, remaining);
        }
    } else {
        auto scalar = duckdb::VariantUtils::ConvertVariantToValue(variant, 0, index);
        duckdb::MemoryStream bytes;
        duckdb::BinarySerializer::Serialize(scalar, bytes, options);
        append(output, bytes.GetData(), bytes.GetPosition());
    }
}
static void content(const duckdb::Value &value, std::string &output,
                    const duckdb::SerializationOptions &options, size_t depth, size_t &remaining) {
    visit(depth, remaining);
    duckdb::MemoryStream type;
    duckdb::BinarySerializer::Serialize(value.type(), type, options);
    append(output, type.GetData(), type.GetPosition());
    output.push_back(value.IsNull() ? 1 : 0);
    if (value.IsNull()) return;
    if (value.type().id() == duckdb::LogicalTypeId::VARIANT) {
        duckdb::RecursiveUnifiedVectorFormat format;
#ifdef NATIVE_VALUE_RELEASE
        duckdb::Vector vector(value);
        duckdb::Vector::RecursiveToUnifiedFormat(vector, 1, format);
#else
        duckdb::Vector vector(value, duckdb::count_t(1));
        duckdb::Vector::RecursiveToUnifiedFormat(vector, format);
#endif
        duckdb::UnifiedVariantVectorData variant(format);
        variant_content(variant, 0, output, options, depth + 1, remaining);
        return;
    }
    const duckdb::vector<duckdb::Value> *children = nullptr;
    switch (value.type().InternalType()) {
    case duckdb::PhysicalType::LIST: children = &duckdb::ListValue::GetChildren(value); break;
    case duckdb::PhysicalType::ARRAY: children = &duckdb::ArrayValue::GetChildren(value); break;
    case duckdb::PhysicalType::STRUCT: children = &duckdb::StructValue::GetChildren(value); break;
    default: break;
    }
    if (children) {
        uint64_t count = children->size();
        append(output, &count, sizeof(count));
        for (auto &child : *children) content(child, output, options, depth + 1, remaining);
    } else {
        duckdb::MemoryStream bytes;
        duckdb::BinarySerializer::Serialize(value, bytes, options);
        append(output, bytes.GetData(), bytes.GetPosition());
    }
}
int main(int argc, char **argv) {
    try {
        if (argc != 3) throw std::runtime_error("usage: helper encode|decode version (input on stdin)");
        if (std::string(argv[1]) == "identity") {
            std::cout << duckdb::DuckDB::LibraryVersion() << "\n" << duckdb::DuckDB::SourceID() << "\n";
            return 0;
        }
        std::string input((std::istreambuf_iterator<char>(std::cin)), std::istreambuf_iterator<char>());
        duckdb::Value value;
        if (std::string(argv[1]) == "encode") {
            duckdb::DuckDB database(nullptr);
            duckdb::Connection connection(database);
            auto result = connection.Query("SELECT " + input);
            if (result->HasError()) throw std::runtime_error(result->GetError());
            if (result->RowCount() != 1 || result->ColumnCount() != 1) throw std::runtime_error("one value required");
            value = result->GetValue(0, 0);
        } else if (std::string(argv[1]) == "decode") {
            if (input.size() % 2) throw std::runtime_error("odd hexadecimal input");
            std::vector<uint8_t> bytes;
            for (size_t i = 0; i < input.size(); i += 2) bytes.push_back(digit(input[i]) * 16 + digit(input[i + 1]));
            duckdb::MemoryStream stream(bytes.data(), bytes.size());
            duckdb::BinaryDeserializer deserializer(stream);
            deserializer.Begin();
            value = duckdb::Value::Deserialize(deserializer);
            deserializer.End();
            if (stream.GetPosition() != bytes.size()) throw std::runtime_error("trailing serialized input");
        } else throw std::runtime_error("unknown mode");
        duckdb::SerializationOptions options;
#ifdef NATIVE_VALUE_RELEASE
        options.serialization_compatibility = duckdb::SerializationCompatibility::FromString(argv[2]);
#else
        options.storage_compatibility = duckdb::StorageCompatibility::FromString(argv[2]);
#endif
        duckdb::MemoryStream output;
        duckdb::BinarySerializer::Serialize(value, output, options);
        std::string type = value.type().ToString();
        std::cout << hex(reinterpret_cast<const uint8_t *>(type.data()), type.size()) << "\n";
        std::cout << hex(output.GetData(), output.GetPosition()) << "\n";
        std::string exact;
        size_t remaining = 16777216;
        content(value, exact, options, 0, remaining);
        std::cout << hex(reinterpret_cast<const uint8_t *>(exact.data()), exact.size()) << "\n";
        return 0;
    } catch (std::exception &error) {
        std::cerr << error.what() << "\n";
        return 1;
    }
}
