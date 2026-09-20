// One-operation pinned FileSystem worker. Parent fixture creation/result checks
// are outside this process, matching the Rust worker boundary.
#include "duckdb.hpp"
#include "duckdb/common/file_system.hpp"
#include "duckdb/common/file_open_flags.hpp"
#include <chrono>
#include <fstream>
#include <iostream>
#include <vector>
using namespace duckdb;
static std::string Parent(const std::string &p) { auto n=p.find_last_of("/\\"); return n==std::string::npos?".":(n==0?"/":p.substr(0,n)); }
static std::vector<char> Seed(const std::string &p, idx_t n) { std::ifstream f(p+".seed",std::ios::binary); std::vector<char>b(n); f.read(b.data(),static_cast<std::streamsize>(n)); if(!f||f.gcount()!=static_cast<std::streamsize>(n)) throw std::runtime_error("fixture"); return b; }
static uint64_t Checksum(const std::vector<char> &b) { uint64_t v=0; for(auto c:b) v=v*257+static_cast<unsigned char>(c); return v; }
int main(int argc,char **argv) {
 if(argc!=4)return 2; std::string op(argv[1]),path(argv[2]); idx_t n; try{n=static_cast<idx_t>(std::stoull(argv[3]));}catch(...){return 2;}
 if(!n||n>512ULL*1024*1024||!(op=="sequential-read"||op=="positioned-read"||op=="publication"||op=="publication-cleanup"))return 2;
 auto fs=FileSystem::CreateLocal(); std::vector<char> seed; uint64_t checksum=0; try { if(op=="publication"||op=="publication-cleanup") seed=Seed(path,n); auto started=std::chrono::steady_clock::now();
		if(op=="sequential-read"||op=="positioned-read"){auto h=fs->OpenFile(path,FileFlags::FILE_FLAGS_READ);std::vector<char>b(n);if(op=="sequential-read"){idx_t done=0;while(done<n){auto got=h->Read(b.data()+done,n-done);if(got<=0)return 3;done+=static_cast<idx_t>(got);}}else h->Read(b.data(),n,0); auto elapsed=std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now()-started).count();checksum=Checksum(b);std::cout<<"{\"engine\":\"cpp\",\"source_id\":\""<<DuckDB::SourceID()<<"\",\"operation\":\""<<op<<"\",\"bytes\":"<<n<<",\"elapsed_ns\":"<<elapsed<<",\"checksum\":"<<checksum<<"}\n";return 0;}
 else {std::string stage=path+".h1-stage";auto h=fs->OpenFile(stage,FileFlags::FILE_FLAGS_WRITE|FileFlags::FILE_FLAGS_FILE_CREATE_NEW|FileFlags::FILE_FLAGS_PRIVATE);h->Write(seed.data(),n);if(op=="publication")h->Sync();h.reset();if(op=="publication"){fs->MoveFile(stage,path);auto d=fs->OpenFile(Parent(path),FileFlags::FILE_FLAGS_READ);d->Sync();}else fs->RemoveFile(stage);}
 auto elapsed=std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now()-started).count();std::cout<<"{\"engine\":\"cpp\",\"source_id\":\""<<DuckDB::SourceID()<<"\",\"operation\":\""<<op<<"\",\"bytes\":"<<n<<",\"elapsed_ns\":"<<elapsed<<"}\n";
 }catch(std::exception &){return 4;} return 0;
}
