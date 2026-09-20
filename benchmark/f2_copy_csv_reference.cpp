#include "duckdb.hpp"
#include <chrono>
#include <fstream>
#include <iostream>
using namespace duckdb;
int main(int argc,char **argv){if(argc!=4)return 2;std::string path=argv[1],opts=argv[3];idx_t rows=std::stoull(argv[2]);DuckDB db(nullptr);Connection c(db);if(c.Query("CREATE TABLE f2(i INTEGER,note VARCHAR)")->HasError())return 3;for(idx_t i=0;i<rows;i++)if(c.Query("INSERT INTO f2 VALUES ("+std::to_string(i)+",'row,"+std::to_string(i)+" \"quoted\"')")->HasError())return 3;auto start=std::chrono::steady_clock::now();auto r=c.Query("COPY f2 TO '"+path+"'"+opts);if(r->HasError())return 4;std::ifstream f(path,std::ios::binary);uint64_t h=0;char b;size_t n=0;while(f.get(b)){h=h*257+static_cast<unsigned char>(b);n++;}auto ns=std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now()-start).count();std::cout<<"{\"engine\":\"cpp\",\"source_id\":\""<<DuckDB::SourceID()<<"\",\"rows\":"<<rows<<",\"written\":"<<rows<<",\"bytes\":"<<n<<",\"elapsed_ns\":"<<ns<<",\"hash\":"<<h<<"}\n";}
