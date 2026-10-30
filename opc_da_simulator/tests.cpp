#include "opc_da.hpp"
#include "json.hpp"
#include <cmath>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {
#define REQUIRE(condition) do { if(!(condition)) throw std::runtime_error(std::string("assertion failed at line ")+std::to_string(__LINE__)+": "+#condition); } while(false)
void check_hresult(HRESULT hr, int line, const char* expression) {
    if (FAILED(hr)) throw std::runtime_error(std::string(expression) + " failed at line " +
        std::to_string(line) + " with HRESULT " + std::to_string(hr));
}
#define ok(expression) check_hresult((expression), __LINE__, #expression)
template<class T> struct Ptr {
    T* p=nullptr;
    ~Ptr() { if(p) p->Release(); }
    T* operator->() const { return p; }
    void** address() { return reinterpret_cast<void**>(&p); }
};
struct ReadResult {
    DWORD count; OPCITEMSTATE* values=nullptr; HRESULT* errors=nullptr;
    explicit ReadResult(DWORD n):count(n) {}
    ~ReadResult() { if(values) for(DWORD i=0;i<count;++i) VariantClear(&values[i].vDataValue); CoTaskMemFree(values); CoTaskMemFree(errors); }
};
void free_attrs(OPCITEMATTRIBUTES* values,ULONG n) {
    for(ULONG i=0;i<n;++i) { CoTaskMemFree(values[i].szAccessPath); CoTaskMemFree(values[i].szItemID); CoTaskMemFree(values[i].pBlob); VariantClear(&values[i].vEUInfo); }
    CoTaskMemFree(values);
}
void json_tests() {
    auto data=json::Parser(R"({"id":"sensor\u6e29\u5ea6\ud83c\udf21","v":-1.25e2,"b":true,"a":[null,false,"\"\\\n"]})").parse();
    REQUIRE(data.at("v").numeric()==-125);
    REQUIRE(data.at("id").text()=="sensor\xe6\xb8\xa9\xe5\xba\xa6\xf0\x9f\x8c\xa1");
    REQUIRE(json::Parser(json::dump(data)).parse().at("a").elements().size()==3);
    const std::vector<std::string> invalid={"", "01", "-", "1.", "1e", "1e9999", "[1,]", "{\"a\":1,\"a\":2}", "true false", "\"\\ud800\"", "\"\\udc00\"", "\"\\u000g\"", std::string("\"\xc0\x80\""), std::string("\"\xed\xa0\x80\"")};
    for(const auto& input:invalid) {
        bool rejected=false; try { json::Parser(input).parse(); } catch(const std::exception&) { rejected=true; }
        REQUIRE(rejected);
    }
    REQUIRE(json::Parser("1.2345678901234567").parse().numeric()==1.2345678901234567);
}
void com_tests() {
    Ptr<IOPCServer> server;
    ok(CoCreateInstance(CLSID_AuTHRityDaSimulator,nullptr,CLSCTX_INPROC_SERVER,IID_IOPCServer,server.address()));
    Ptr<ISimulatorControl> control; ok(server->QueryInterface(IID_ISimulatorControl,control.address()));
    Ptr<IUnknown> a,b; ok(server->QueryInterface(IID_IUnknown,a.address())); ok(control->QueryInterface(IID_IUnknown,b.address())); REQUIRE(a.p==b.p);
    void* absent=reinterpret_cast<void*>(1); REQUIRE(server->QueryInterface(IID_IDispatch,&absent)==E_NOINTERFACE); REQUIRE(absent==nullptr);
    FILETIME initial=unix_ms_to_filetime(1767225600123LL),next=unix_ms_to_filetime(1767225660123LL);
    REQUIRE(filetime_to_unix_ms(initial)==1767225600123LL);
    SimItem config[]={
        {L"Sensor.\u6e29\u5ea6",12.5,TRUE,FALSE,OPC_QUALITY_GOOD,initial},
        {L"Actuator:arbitrary",1.25,TRUE,TRUE,OPC_QUALITY_GOOD,initial},
        {L"WriteOnly",4.0,FALSE,TRUE,OPC_QUALITY_GOOD,initial}
    };
    ok(control->Configure(3,config,initial));
    REQUIRE(FAILED(control->Configure(3,config,initial)));
    Ptr<IOPCItemMgt> items; OPCHANDLE group=0; DWORD revised=0;
    REQUIRE(server->AddGroup(L"test",TRUE,0,101,nullptr,nullptr,0,&group,&revised,IID_IOPCItemMgt,reinterpret_cast<LPUNKNOWN*>(&items.p))==OPC_S_UNSUPPORTEDRATE);
    REQUIRE(group!=0&&revised==1);
    IUnknown* duplicate=nullptr; OPCHANDLE h; DWORD rate;
    REQUIRE(server->AddGroup(L"test",TRUE,1,0,nullptr,nullptr,0,&h,&rate,IID_IOPCItemMgt,&duplicate)==OPC_E_DUPLICATENAME);
    REQUIRE(duplicate==nullptr);
    Ptr<IOPCSyncIO> sync; Ptr<IOPCGroupStateMgt> state;
    ok(items->QueryInterface(IID_IOPCSyncIO,sync.address())); ok(sync->QueryInterface(IID_IOPCGroupStateMgt,state.address()));
    Ptr<IUnknown> ia,ib; ok(items->QueryInterface(IID_IUnknown,ia.address())); ok(sync->QueryInterface(IID_IUnknown,ib.address())); REQUIRE(ia.p==ib.p);
    Ptr<IOPCItemMgt> by_name; ok(server->GetGroupByName(L"test",IID_IOPCItemMgt,reinterpret_cast<LPUNKNOWN*>(&by_name.p))); REQUIRE(items.p==by_name.p);
    OPCITEMDEF definitions[6]{};
    for(int i=0;i<3;++i) { definitions[i].szItemID=const_cast<LPWSTR>(config[i].id); definitions[i].bActive=TRUE; definitions[i].hClient=200+i; definitions[i].vtRequestedDataType=VT_R8; }
    definitions[3]=definitions[0]; definitions[3].szItemID=const_cast<LPWSTR>(L"missing");
    definitions[4]=definitions[0]; definitions[4].vtRequestedDataType=VT_ARRAY|VT_R8;
    definitions[5]=definitions[0]; definitions[5].szAccessPath=const_cast<LPWSTR>(L"unsupported");
    OPCITEMRESULT* results=nullptr; HRESULT* errors=nullptr;
    REQUIRE(items->ValidateItems(6,definitions,TRUE,&results,&errors)==S_FALSE);
    REQUIRE(results[0].hServer==0&&results[0].vtCanonicalDataType==VT_R8);
    REQUIRE(results[0].dwAccessRights==OPC_READABLE&&results[1].dwAccessRights==(OPC_READABLE|OPC_WRITEABLE));
    REQUIRE(errors[3]==OPC_E_UNKNOWNITEMID&&errors[4]==OPC_E_BADTYPE&&errors[5]==OPC_E_UNKNOWNPATH);
    CoTaskMemFree(results); CoTaskMemFree(errors);
    REQUIRE(items->AddItems(6,definitions,&results,&errors)==S_FALSE);
    OPCHANDLE handles[4]={results[0].hServer,results[1].hServer,results[2].hServer,0};
    REQUIRE(handles[0]!=handles[1]&&handles[1]!=handles[2]);
    CoTaskMemFree(results); CoTaskMemFree(errors);
    {
        ReadResult r(4); REQUIRE(sync->Read(OPC_DS_DEVICE,4,handles,&r.values,&r.errors)==S_FALSE);
        REQUIRE(r.errors[0]==S_OK&&r.errors[1]==S_OK&&r.errors[2]==OPC_E_BADRIGHTS&&r.errors[3]==OPC_E_INVALIDHANDLE);
        REQUIRE(r.values[0].hClient==200&&r.values[0].vDataValue.vt==VT_R8&&r.values[0].vDataValue.dblVal==12.5);
        REQUIRE(r.values[0].wQuality==OPC_QUALITY_GOOD&&filetime_to_unix_ms(r.values[0].ftTimeStamp)==1767225600123LL);
    }
    VARIANT write[3]{}; for(int i=0;i<3;++i) { write[i].vt=VT_R8; write[i].dblVal=20+i; }
    ok(control->SetClock(next)); REQUIRE(sync->Write(3,handles,write,&errors)==S_FALSE);
    REQUIRE(errors[0]==OPC_E_BADRIGHTS&&errors[1]==S_OK&&errors[2]==S_OK); CoTaskMemFree(errors);
    {
        ReadResult r(1); ok(sync->Read(OPC_DS_DEVICE,1,handles+1,&r.values,&r.errors));
        REQUIRE(r.values[0].vDataValue.dblVal==21&&filetime_to_unix_ms(r.values[0].ftTimeStamp)==1767225660123LL);
    }
    VARTYPE requested=VT_BSTR; ok(items->SetDatatypes(1,handles+1,&requested,&errors)); REQUIRE(errors[0]==S_OK); CoTaskMemFree(errors);
    {
        ReadResult r(1); ok(sync->Read(OPC_DS_DEVICE,1,handles+1,&r.values,&r.errors)); REQUIRE(r.values[0].vDataValue.vt==VT_BSTR);
        REQUIRE(std::wstring(r.values[0].vDataValue.bstrVal)==L"21");
    }
    requested=VT_R8; ok(items->SetDatatypes(1,handles+1,&requested,&errors)); CoTaskMemFree(errors);
    requested=VT_UNKNOWN; REQUIRE(items->SetDatatypes(1,handles+1,&requested,&errors)==S_FALSE); REQUIRE(errors[0]==OPC_E_BADTYPE); CoTaskMemFree(errors);
    OPCHANDLE new_client=999; ok(items->SetClientHandles(1,handles,&new_client,&errors)); CoTaskMemFree(errors);
    ok(items->SetActiveState(1,handles,FALSE,&errors)); CoTaskMemFree(errors);
    {
        ReadResult cached(1),device(1); ok(sync->Read(OPC_DS_CACHE,1,handles,&cached.values,&cached.errors));
        ok(sync->Read(OPC_DS_DEVICE,1,handles,&device.values,&device.errors));
        REQUIRE(cached.values[0].wQuality==OPC_QUALITY_OUT_OF_SERVICE&&device.values[0].wQuality==OPC_QUALITY_GOOD);
        REQUIRE(device.values[0].hClient==999);
    }
    ok(items->SetActiveState(1,handles,TRUE,&errors)); CoTaskMemFree(errors);
    DWORD update=25,locale=0x409; BOOL active=FALSE; LONG bias=30; FLOAT deadband=5; OPCHANDLE client=123;
    ok(state->SetState(&update,&revised,&active,&bias,&deadband,&locale,&client)); REQUIRE(revised==25);
    LPWSTR name=nullptr; DWORD got_rate,got_locale; BOOL got_active; LONG got_bias; FLOAT got_deadband; OPCHANDLE got_client,got_handle;
    ok(state->GetState(&got_rate,&got_active,&name,&got_bias,&got_deadband,&got_locale,&got_client,&got_handle));
    REQUIRE(std::wstring(name)==L"test"&&got_rate==25&&!got_active&&got_bias==30&&got_deadband==5&&got_locale==0x409&&got_client==123&&got_handle==group); CoTaskMemFree(name);
    active=TRUE; ok(state->SetState(nullptr,&revised,&active,nullptr,nullptr,nullptr,nullptr));
    deadband=101; REQUIRE(state->SetState(nullptr,&revised,nullptr,nullptr,&deadband,nullptr,nullptr)==E_INVALIDARG);
    ok(state->SetName(L"renamed"));
    Ptr<IOPCGroupStateMgt> clone; ok(state->CloneGroup(L"clone",IID_IOPCGroupStateMgt,reinterpret_cast<LPUNKNOWN*>(&clone.p)));
    ok(clone->GetState(&got_rate,&got_active,&name,&got_bias,&got_deadband,&got_locale,&got_client,&got_handle)); REQUIRE(!got_active&&got_handle!=group); CoTaskMemFree(name);
    OPCHANDLE clone_handle=got_handle;
    {
        Ptr<IEnumOPCItemAttributes> enumeration;
        ok(items->CreateEnumerator(IID_IEnumOPCItemAttributes,reinterpret_cast<LPUNKNOWN*>(&enumeration.p)));
        OPCITEMATTRIBUTES* attrs=nullptr; ULONG fetched=0;
        ok(enumeration->Next(1,&attrs,&fetched)); REQUIRE(fetched==1&&attrs[0].hServer==handles[0]&&attrs[0].hClient==999); free_attrs(attrs,fetched);
        Ptr<IEnumOPCItemAttributes> copy; ok(enumeration->Clone(&copy.p));
        ok(enumeration->Skip(1)); REQUIRE(enumeration->Next(5,&attrs,&fetched)==S_FALSE); REQUIRE(fetched==1); free_attrs(attrs,fetched);
        REQUIRE(copy->Next(5,&attrs,&fetched)==S_FALSE); REQUIRE(fetched==2); free_attrs(attrs,fetched);
        ok(enumeration->Reset()); REQUIRE(enumeration->Skip(4)==S_FALSE);
    }
    {
        Ptr<IEnumString> names; ok(server->CreateGroupEnumerator(OPC_ENUM_PRIVATE,IID_IEnumString,reinterpret_cast<LPUNKNOWN*>(&names.p)));
        LPWSTR names_out[3]{}; ULONG n=0; REQUIRE(names->Next(3,names_out,&n)==S_FALSE); REQUIRE(n==2);
        for(ULONG i=0;i<n;++i) CoTaskMemFree(names_out[i]);
        ok(names->Reset()); Ptr<IEnumString> copy; ok(names->Clone(&copy.p)); ok(copy->Skip(2)); REQUIRE(copy->Skip(1)==S_FALSE);
        Ptr<IEnumString> public_names; ok(server->CreateGroupEnumerator(OPC_ENUM_PUBLIC,IID_IEnumString,reinterpret_cast<LPUNKNOWN*>(&public_names.p)));
        REQUIRE(public_names->Next(1,names_out,&n)==S_FALSE&&n==0);
        Ptr<IEnumUnknown> groups; ok(server->CreateGroupEnumerator(OPC_ENUM_ALL_CONNECTIONS,IID_IEnumUnknown,reinterpret_cast<LPUNKNOWN*>(&groups.p)));
        IUnknown* objects[3]{}; REQUIRE(groups->Next(3,objects,&n)==S_FALSE&&n==2); for(ULONG i=0;i<n;++i) objects[i]->Release();
        ok(groups->Reset()); Ptr<IEnumUnknown> group_copy; ok(groups->Clone(&group_copy.p)); ok(group_copy->Skip(2)); REQUIRE(group_copy->Skip(1)==S_FALSE);
    }
    LPCWSTR sensor=config[0].id,actuator=config[1].id;
    ok(control->SetFault(SIM_BAD_QUALITY,TRUE,1,&sensor));
    SimUpdate measured{sensor,31.75,OPC_QUALITY_GOOD,next}; ok(control->Update(1,&measured,next));
    {
        ReadResult r(1); ok(sync->Read(OPC_DS_DEVICE,1,handles,&r.values,&r.errors));
        REQUIRE(r.errors[0]==S_OK&&r.values[0].wQuality==OPC_QUALITY_BAD&&r.values[0].vDataValue.dblVal==31.75);
    }
    ok(control->SetFault(SIM_BAD_QUALITY,FALSE,1,&sensor));
    ok(control->SetFault(SIM_WRITE_DENIED,TRUE,1,&actuator));
    REQUIRE(sync->Write(1,handles+1,write+1,&errors)==S_FALSE&&errors[0]==OPC_E_BADRIGHTS); CoTaskMemFree(errors);
    ok(control->SetFault(SIM_WRITE_DENIED,FALSE,1,&actuator));
    ok(control->SetFault(SIM_DISCONNECT,TRUE,0,nullptr));
    {
        ReadResult r(1); REQUIRE(sync->Read(OPC_DS_DEVICE,1,handles,&r.values,&r.errors)==RPC_E_DISCONNECTED); REQUIRE(!r.values&&!r.errors);
        OPCSERVERSTATUS* status=nullptr; ok(server->GetStatus(&status)); REQUIRE(status->dwServerState==OPC_STATUS_COMM_FAULT); CoTaskMemFree(status->szVendorInfo); CoTaskMemFree(status);
    }
    ok(control->SetFault(SIM_DISCONNECT,FALSE,0,nullptr));
    write[1].vt=VT_BSTR; write[1].bstrVal=SysAllocString(L"42.5"); ok(sync->Write(1,handles+1,write+1,&errors)); REQUIRE(errors[0]==S_OK); CoTaskMemFree(errors); VariantClear(write+1);
    write[1].vt=VT_R8; write[1].dblVal=INFINITY; REQUIRE(sync->Write(1,handles+1,write+1,&errors)==S_FALSE&&errors[0]==OPC_E_RANGE); CoTaskMemFree(errors);
    ok(items->RemoveItems(1,handles+2,&errors)); CoTaskMemFree(errors);
    {
        ReadResult r(1); REQUIRE(sync->Read(OPC_DS_DEVICE,1,handles+2,&r.values,&r.errors)==S_FALSE&&r.errors[0]==OPC_E_INVALIDHANDLE);
    }
    LPWSTR message=nullptr; ok(server->GetErrorString(OPC_E_BADRIGHTS,0,&message)); REQUIRE(message&&*message); CoTaskMemFree(message);
    SimStats stats{}; ok(control->GetStatistics(&stats));
    REQUIRE(stats.read_calls==8&&stats.write_calls==4&&stats.add_items_calls==1&&stats.fault_calls==6);
    REQUIRE(stats.read_errors==4&&stats.write_errors==3&&stats.bad_quality_items==2&&stats.update_calls==1);
    REQUIRE(server->RemoveGroup(group,FALSE)==OPC_S_INUSE);
    ok(server->RemoveGroup(clone_handle,TRUE));
    REQUIRE(clone->GetState(&got_rate,&got_active,&name,&got_bias,&got_deadband,&got_locale,&got_client,&got_handle)==RPC_E_DISCONNECTED);
    ok(server->RemoveGroup(group,TRUE)); REQUIRE(server->RemoveGroup(group,TRUE)==OPC_E_INVALIDHANDLE);
    std::cout<<"COM calls: read="<<stats.read_calls<<" write="<<stats.write_calls<<" bad-quality="<<stats.bad_quality_items<<" read-errors="<<stats.read_errors<<" write-errors="<<stats.write_errors<<'\n';
}
}
int main() {
    HRESULT hr=CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED);
    if(FAILED(hr)) { std::cerr<<"COM initialization failed\n"; return 1; }
    int result=0;
    try { json_tests(); com_tests(); std::cout<<"Native JSON and synchronous OPC DA COM tests passed\n"; }
    catch(const std::exception& error) { std::cerr<<error.what()<<'\n'; result=1; }
    CoUninitialize(); return result;
}
