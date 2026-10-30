#include "opc_da.hpp"
#include "json.hpp"
#include <fcntl.h>
#include <io.h>
#include <iostream>
#include <map>
#include <memory>
#include <set>
#include <string>
#include <vector>

namespace {
using json::Value;
struct ComError: std::runtime_error {
    HRESULT code;
    ComError(const std::string& operation,HRESULT hr):std::runtime_error(operation+" failed (HRESULT "+std::to_string(static_cast<long>(hr))+")"),code(hr) {}
};
void check(HRESULT hr,const char* operation) { if(FAILED(hr)) throw ComError(operation,hr); }
template<class T> struct ComPtr {
    T* p=nullptr;
    ComPtr()=default; ComPtr(const ComPtr&)=delete; ComPtr& operator=(const ComPtr&)=delete;
    ~ComPtr() { reset(); }
    void reset() { if(p) p->Release(); p=nullptr; }
    T* operator->() const { return p; }
    void** address() { return reinterpret_cast<void**>(&p); }
};
struct TaskMemory {
    void* p=nullptr;
    explicit TaskMemory(void* value):p(value) {}
    ~TaskMemory() { CoTaskMemFree(p); }
};
struct ReadMemory {
    OPCITEMSTATE* values=nullptr; HRESULT* errors=nullptr; DWORD count=0;
    ~ReadMemory() {
        if(values) for(DWORD i=0;i<count;++i) VariantClear(&values[i].vDataValue);
        CoTaskMemFree(values); CoTaskMemFree(errors);
    }
};
std::wstring widen(const std::string& s) {
    if(s.empty()||s.size()>32767||s.find('\0')!=std::string::npos) throw std::runtime_error("item ID must be nonempty UTF-8 without NUL, at most 32767 bytes");
    int size=MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,s.data(),static_cast<int>(s.size()),nullptr,0);
    if(!size) throw std::runtime_error("invalid UTF-8 item ID");
    std::wstring result(size,L'\0');
    if(!MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,s.data(),static_cast<int>(s.size()),result.data(),size)) throw std::runtime_error("UTF-8 conversion failed");
    return result;
}
std::int64_t timestamp(const Value& request) {
    double n=request.at("timestamp_ms").numeric();
    if(n<0||n>253402300799999.0||std::floor(n)!=n) throw std::runtime_error("timestamp_ms must be an integer Unix millisecond in years 1970..9999");
    return static_cast<std::int64_t>(n);
}
WORD quality(const Value& item) {
    auto* v=item.find("quality"); if(!v) return OPC_QUALITY_GOOD;
    double n=v->numeric(); if(n<0||n>65535||std::floor(n)!=n) throw std::runtime_error("quality must be an unsigned 16-bit integer");
    return static_cast<WORD>(n);
}
DWORD item_count(const std::vector<Value>& values) {
    if(values.empty()||values.size()>100000) throw std::runtime_error("items must contain 1..100000 entries");
    return static_cast<DWORD>(values.size());
}
Value success() { auto out=Value::dict(); out.object.emplace("ok",Value(true)); return out; }
Value numeric(HRESULT hr) { return Value(static_cast<double>(static_cast<long>(hr))); }
Value statistics(const SimStats& stats) {
    auto out=Value::dict();
#define STAT(name) out.object.emplace(#name,Value(static_cast<double>(stats.name)))
    STAT(query_interface_calls); STAT(add_items_calls); STAT(read_calls); STAT(write_calls);
    STAT(read_items); STAT(write_items); STAT(read_errors); STAT(write_errors); STAT(bad_quality_items);
    STAT(update_calls); STAT(fault_calls); STAT(read_nanoseconds); STAT(write_nanoseconds);
#undef STAT
    return out;
}
class Bridge {
    ComPtr<IOPCServer> server;
    ComPtr<ISimulatorControl> control;
    ComPtr<IOPCItemMgt> item_management;
    ComPtr<IOPCSyncIO> sync;
    OPCHANDLE group=0;
    struct Entry { std::wstring wide; OPCHANDLE handle=0; };
    std::map<std::string,Entry> entries;
    bool initialized=false;
public:
    ~Bridge() { reset(); }
    void reset() {
        sync.reset(); item_management.reset();
        if(server.p&&group) server->RemoveGroup(group,TRUE);
        group=0; control.reset(); server.reset(); entries.clear(); initialized=false;
    }
    Value stats() {
        if(!control.p) return Value::dict();
        SimStats stats{}; check(control->GetStatistics(&stats),"ISimulatorControl::GetStatistics"); return statistics(stats);
    }
    void initialize(const Value& request) {
        if(initialized) throw std::runtime_error("bridge is already initialized");
        const auto& items=request.at("items").elements(); DWORD count=item_count(items);
        FILETIME clock=unix_ms_to_filetime(timestamp(request));
        std::vector<SimItem> config; config.reserve(count);
        try {
            for(const auto& item:items) {
                const auto& id=item.at("id").text();
                auto inserted=entries.emplace(id,Entry{widen(id),0});
                if(!inserted.second) throw std::runtime_error("duplicate item ID: "+id);
                const auto* readable=item.find("readable");
                config.push_back(SimItem{inserted.first->second.wide.c_str(),item.at("value").numeric(),readable?readable->truth():true,item.at("writable").truth(),quality(item),clock});
            }
            CLSID registered;
            check(CLSIDFromProgID(SIM_PROGID,&registered),"CLSIDFromProgID");
            if(registered!=CLSID_AuTHRityDaSimulator) throw std::runtime_error("isolated ProgID does not match simulator CLSID");
            check(CoCreateInstance(registered,nullptr,CLSCTX_INPROC_SERVER,IID_IOPCServer,server.address()),"CoCreateInstance(IOPCServer)");
            check(server->QueryInterface(IID_ISimulatorControl,control.address()),"QueryInterface(ISimulatorControl)");
            check(control->Configure(count,config.data(),clock),"ISimulatorControl::Configure");
            DWORD revised=0;
            check(server->AddGroup(L"Campaign",TRUE,1,1,nullptr,nullptr,0,&group,&revised,IID_IOPCItemMgt,reinterpret_cast<LPUNKNOWN*>(&item_management.p)),"IOPCServer::AddGroup");
            check(item_management->QueryInterface(IID_IOPCSyncIO,sync.address()),"QueryInterface(IOPCSyncIO)");
            std::vector<OPCITEMDEF> definitions(count);
            for(DWORD i=0;i<count;++i) {
                definitions[i].szItemID=const_cast<LPWSTR>(config[i].id);
                definitions[i].bActive=TRUE; definitions[i].hClient=i+1; definitions[i].vtRequestedDataType=VT_R8;
            }
            OPCITEMRESULT* result=nullptr; HRESULT* errors=nullptr;
            HRESULT hr=item_management->AddItems(count,definitions.data(),&result,&errors);
            TaskMemory result_memory(result),error_memory(errors);
            check(hr,"IOPCItemMgt::AddItems");
            for(DWORD i=0;i<count;++i) {
                check(errors[i],"IOPCItemMgt::AddItems item");
                entries.at(items[i].at("id").text()).handle=result[i].hServer;
                CoTaskMemFree(result[i].pBlob);
            }
            initialized=true;
        } catch(...) { reset(); throw; }
    }
    void require_initialized() const { if(!initialized) throw std::runtime_error("init must succeed before this operation"); }
    std::vector<OPCHANDLE> handles(const std::vector<Value>& items,bool objects) const {
        std::vector<OPCHANDLE> out; out.reserve(items.size());
        for(const auto& item:items) {
            const std::string& id=objects?item.at("id").text():item.text(); auto it=entries.find(id);
            // Invalid IDs reach the real DA method as invalid handles, rather than fake client statuses.
            out.push_back(it==entries.end()?0:it->second.handle);
        }
        return out;
    }
    Value execute(const Value& request,bool& close) {
        const auto& op=request.at("op").text(); Value response=success();
        if(op=="init") {
            initialize(request);
            response.object.emplace("profile",Value("local-inproc-opc-da-synchronous"));
            response.object.emplace("progid",Value("AuTHRity.LocalDaSimulator.1"));
        } else if(op=="close") {
            response.object.emplace("stats",stats()); reset(); close=true; return response;
        } else {
            require_initialized();
            if(op=="read") {
                const auto& items=request.at("items").elements(); DWORD count=item_count(items);
                auto server_handles=handles(items,false); ReadMemory memory; memory.count=count;
                HRESULT hr=sync->Read(OPC_DS_DEVICE,count,server_handles.data(),&memory.values,&memory.errors);
                check(hr,"IOPCSyncIO::Read");
                auto values=Value::list(),qualities=Value::list(),times=Value::list(),errors=Value::list();
                values.array.reserve(count); qualities.array.reserve(count); times.array.reserve(count); errors.array.reserve(count);
                for(DWORD i=0;i<count;++i) {
                    const bool ok=SUCCEEDED(memory.errors[i]); const auto& item=memory.values[i];
                    if(ok&&item.vDataValue.vt!=VT_R8) throw std::runtime_error("DA server returned a non-R8 requested variant");
                    values.array.emplace_back(ok?Value(item.vDataValue.dblVal):Value());
                    qualities.array.emplace_back(static_cast<double>(ok?item.wQuality:0));
                    times.array.emplace_back(static_cast<double>(ok?filetime_to_unix_ms(item.ftTimeStamp):0));
                    errors.array.push_back(numeric(memory.errors[i]));
                }
                response.object.emplace("values",std::move(values)); response.object.emplace("quality",std::move(qualities));
                response.object.emplace("timestamp_ms",std::move(times)); response.object.emplace("hresult",std::move(errors));
                response.object.emplace("call_hresult",numeric(hr));
            } else if(op=="write") {
                const auto& items=request.at("items").elements(); DWORD count=item_count(items);
                FILETIME clock=unix_ms_to_filetime(timestamp(request));
                auto server_handles=handles(items,true); std::vector<VARIANT> values(count);
                for(DWORD i=0;i<count;++i) { values[i].vt=VT_R8; values[i].dblVal=items[i].at("value").numeric(); }
                check(control->SetClock(clock),"ISimulatorControl::SetClock");
                HRESULT* errors=nullptr; HRESULT hr=sync->Write(count,server_handles.data(),values.data(),&errors);
                TaskMemory memory(errors); check(hr,"IOPCSyncIO::Write"); auto result=Value::list(); result.array.reserve(count);
                for(DWORD i=0;i<count;++i) result.array.push_back(numeric(errors[i]));
                response.object.emplace("hresult",std::move(result)); response.object.emplace("call_hresult",numeric(hr));
            } else if(op=="update") {
                const auto& items=request.at("items").elements(); DWORD count=item_count(items);
                FILETIME clock=unix_ms_to_filetime(timestamp(request)); std::vector<SimUpdate> updates; updates.reserve(count);
                std::vector<std::wstring> unknown; unknown.reserve(count);
                for(const auto& item:items) {
                    const auto& id=item.at("id").text(); auto entry=entries.find(id); LPCWSTR wide;
                    if(entry==entries.end()) { unknown.push_back(widen(id)); wide=unknown.back().c_str(); }
                    else wide=entry->second.wide.c_str();
                    FILETIME item_clock=clock;
                    if(item.find("timestamp_ms")) item_clock=unix_ms_to_filetime(timestamp(item));
                    updates.push_back(SimUpdate{wide,item.at("value").numeric(),quality(item),item_clock});
                }
                check(control->Update(count,updates.data(),clock),"ISimulatorControl::Update");
            } else if(op=="fault") {
                const auto& kind=request.at("kind").text(); SimFault fault;
                if(kind=="bad_quality") fault=SIM_BAD_QUALITY; else if(kind=="disconnect") fault=SIM_DISCONNECT;
                else if(kind=="write_denied") fault=SIM_WRITE_DENIED; else throw std::runtime_error("unknown fault kind");
                bool enabled=request.at("enabled").truth(); std::vector<std::wstring> ids; std::vector<LPCWSTR> pointers;
                if(const auto* selected=request.find("items")) {
                    const auto& items=selected->elements(); if(items.empty()) throw std::runtime_error("omit items for an all-item fault; explicit items must not be empty");
                    item_count(items); ids.reserve(items.size()); pointers.reserve(items.size());
                    for(const auto& id:items) ids.push_back(widen(id.text()));
                    for(const auto& id:ids) pointers.push_back(id.c_str());
                }
                check(control->SetFault(fault,enabled,static_cast<DWORD>(pointers.size()),pointers.data()),"ISimulatorControl::SetFault");
            } else if(op!="stats") throw std::runtime_error("unknown operation: "+op);
        }
        response.object.emplace("stats",stats()); return response;
    }
};
// Consume one bounded JSONL record without an unbounded getline allocation.
bool read_line(std::string& line) {
    constexpr size_t max_line=8*1024*1024; line.clear(); bool overflow=false; char c;
    while(std::cin.get(c)) {
        if(c=='\n') {
            if(overflow) throw std::runtime_error("JSONL record exceeds 8 MiB"); return true;
        }
        if(line.size()<max_line) line+=c; else overflow=true;
    }
    if(overflow) throw std::runtime_error("JSONL record exceeds 8 MiB"); return !line.empty();
}
}
int main() {
    _setmode(_fileno(stdin),_O_BINARY); _setmode(_fileno(stdout),_O_BINARY);
    std::ios::sync_with_stdio(false); std::cin.tie(nullptr); std::cout.imbue(std::locale::classic());
    HRESULT apartment=CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED);
    if(FAILED(apartment)) { std::cerr<<"CoInitializeEx failed: "<<static_cast<long>(apartment)<<'\n'; return 2; }
    {
        Bridge bridge; bool close=false; std::string line;
        while(!close) {
            Value response;
            try {
                if(!read_line(line)) break;
                response=bridge.execute(json::Parser(line).parse(),close);
            } catch(const std::exception& error) {
                response=Value::dict(); response.object.emplace("ok",Value(false)); response.object.emplace("error",Value(error.what()));
                const auto* com=dynamic_cast<const ComError*>(&error);
                response.object.emplace("hresult",numeric(com?com->code:E_INVALIDARG));
                response.object.emplace("stats",bridge.stats());
            }
            json::serialize(std::cout,response); std::cout<<'\n'<<std::flush;
            if(!std::cout) break;
        }
    }
    CoUninitialize(); return 0;
}
