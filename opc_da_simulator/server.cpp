#include "opc_da.hpp"
#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstring>
#include <limits>
#include <map>
#include <memory>
#include <new>
#include <string>
#include <utility>
#include <vector>

namespace {
std::atomic<long> live_objects{0}, server_locks{0};
HMODULE module_handle=nullptr;
#define COM_CATCH catch(const std::bad_alloc&) { return E_OUTOFMEMORY; } catch(...) { return E_FAIL; }
struct RefCount {
    ULONG references=1;
    RefCount() { ++live_objects; }
    virtual ~RefCount() { --live_objects; }
    ULONG retain() { return ++references; }
    ULONG release() { ULONG n=--references; if(!n) delete this; return n; }
};
#define COM_REFS ULONG STDMETHODCALLTYPE AddRef() override { return retain(); } \
    ULONG STDMETHODCALLTYPE Release() override { return release(); }
template<class T> struct TaskFree { void operator()(T* p) const { CoTaskMemFree(p); } };
template<class T> using TaskPtr=std::unique_ptr<T,TaskFree<T>>;
template<class T> TaskPtr<T> task_array(size_t n) {
    if(n>std::numeric_limits<size_t>::max()/sizeof(T)) throw std::bad_alloc();
    T* p=static_cast<T*>(CoTaskMemAlloc(n*sizeof(T)));
    if(!p&&n) throw std::bad_alloc();
    if(n) std::memset(p,0,n*sizeof(T)); return TaskPtr<T>(p);
}
LPWSTR copy_string(const std::wstring& s) {
    auto p=task_array<wchar_t>(s.size()+1);
    std::memcpy(p.get(),s.c_str(),(s.size()+1)*sizeof(wchar_t)); return p.release();
}
bool allowed_type(VARTYPE type) {
    switch(type) {
        case VT_EMPTY: case VT_R8: case VT_R4: case VT_I1: case VT_UI1:
        case VT_I2: case VT_UI2: case VT_I4: case VT_UI4: case VT_I8: case VT_UI8:
        case VT_INT: case VT_UINT: case VT_BOOL: case VT_BSTR: case VT_CY: case VT_DECIMAL: return true;
        default: return false;
    }
}
bool valid_locale(DWORD lcid) { return lcid==0 || lcid==LOCALE_SYSTEM_DEFAULT || lcid==LOCALE_USER_DEFAULT || lcid==0x409; }
bool valid_deadband(FLOAT d) { return std::isfinite(d)&&d>=0&&d<=100; }
HRESULT numeric_value(const VARIANT& input,double& out) {
    if(!allowed_type(input.vt)||input.vt==VT_EMPTY) return OPC_E_BADTYPE;
    VARIANT converted; VariantInit(&converted);
    HRESULT hr=VariantChangeType(&converted,const_cast<VARIANT*>(&input),0,VT_R8);
    if(FAILED(hr)) return hr==DISP_E_OVERFLOW?OPC_E_RANGE:OPC_E_BADTYPE;
    out=converted.dblVal; VariantClear(&converted);
    return std::isfinite(out)?S_OK:OPC_E_RANGE;
}
struct Item {
    // Canonical scalar VARIANT owns no heap data; its type never changes.
    VARIANT value{};
    DWORD rights;
    WORD quality;
    FILETIME timestamp;
    bool bad_quality=false,write_denied=false;
    Item(double initial,DWORD access,WORD q,FILETIME time):rights(access),quality(q),timestamp(time) {
        value.vt=VT_R8; value.dblVal=initial;
    }
};
struct Group;
struct State {
    std::map<std::wstring,Item,std::less<>> items;
    std::map<OPCHANDLE,Group*> groups;
    OPCHANDLE next_group=1;
    bool disconnected=false,alive=true,configured=false;
    FILETIME start{},clock{},last_update{};
    SimStats stats{};
    State() { GetSystemTimeAsFileTime(&start); clock=start; last_update=start; }
};
struct Binding { std::wstring id; OPCHANDLE client; BOOL active; VARTYPE requested; };
struct Attribute { Binding binding; OPCHANDLE handle; DWORD rights; };
struct Timer {
    LARGE_INTEGER begin{},frequency{};
    ULONGLONG& total;
    explicit Timer(ULONGLONG& target):total(target) { QueryPerformanceFrequency(&frequency); QueryPerformanceCounter(&begin); }
    ~Timer() { LARGE_INTEGER end; QueryPerformanceCounter(&end); total+=static_cast<ULONGLONG>((end.QuadPart-begin.QuadPart)*1000000000ULL/frequency.QuadPart); }
};
void free_attributes(OPCITEMATTRIBUTES* a,ULONG n) {
    if(!a) return;
    for(ULONG i=0;i<n;++i) { CoTaskMemFree(a[i].szItemID); CoTaskMemFree(a[i].szAccessPath); CoTaskMemFree(a[i].pBlob); VariantClear(&a[i].vEUInfo); }
    CoTaskMemFree(a);
}
class AttributeEnumerator final: public IEnumOPCItemAttributes,public RefCount {
    std::shared_ptr<const std::vector<Attribute>> items;
    size_t cursor=0;
public:
    explicit AttributeEnumerator(std::shared_ptr<const std::vector<Attribute>> data,size_t position=0):items(std::move(data)),cursor(position) {}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override {
        if(!out) return E_POINTER; *out=nullptr;
        if(iid!=IID_IUnknown&&iid!=IID_IEnumOPCItemAttributes) return E_NOINTERFACE;
        *out=static_cast<IEnumOPCItemAttributes*>(this); AddRef(); return S_OK;
    }
    COM_REFS
    HRESULT STDMETHODCALLTYPE Next(ULONG count,OPCITEMATTRIBUTES** out,ULONG* fetched) override try {
        if(!out||(!fetched&&count!=1)) return E_POINTER;
        *out=nullptr; if(fetched) *fetched=0;
        ULONG n=static_cast<ULONG>(std::min<size_t>(count,items->size()-cursor));
        if(!n) return count?S_FALSE:S_OK;
        OPCITEMATTRIBUTES* result=task_array<OPCITEMATTRIBUTES>(n).release();
        try {
            for(ULONG i=0;i<n;++i) {
                const auto& a=(*items)[cursor+i]; auto& r=result[i];
                r.szItemID=copy_string(a.binding.id); r.szAccessPath=copy_string(L"");
                r.bActive=a.binding.active; r.hClient=a.binding.client; r.hServer=a.handle;
                r.dwAccessRights=a.rights; r.vtRequestedDataType=a.binding.requested;
                r.vtCanonicalDataType=VT_R8; r.dwEUType=OPC_NOENUM;
            }
        } catch(...) { free_attributes(result,n); throw; }
        cursor+=n; *out=result; if(fetched) *fetched=n; return n==count?S_OK:S_FALSE;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE Skip(ULONG n) override { size_t k=std::min<size_t>(n,items->size()-cursor); cursor+=k; return k==n?S_OK:S_FALSE; }
    HRESULT STDMETHODCALLTYPE Reset() override { cursor=0; return S_OK; }
    HRESULT STDMETHODCALLTYPE Clone(IEnumOPCItemAttributes** out) override try {
        if(!out) return E_POINTER; *out=nullptr; *out=new AttributeEnumerator(items,cursor); return S_OK;
    } COM_CATCH
};
class StringEnumerator final: public IEnumString,public RefCount {
    std::shared_ptr<const std::vector<std::wstring>> items; size_t cursor=0;
public:
    explicit StringEnumerator(std::shared_ptr<const std::vector<std::wstring>> data,size_t position=0):items(std::move(data)),cursor(position) {}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override {
        if(!out) return E_POINTER; *out=nullptr;
        if(iid!=IID_IUnknown&&iid!=IID_IEnumString) return E_NOINTERFACE;
        *out=static_cast<IEnumString*>(this); AddRef(); return S_OK;
    }
    COM_REFS
    HRESULT STDMETHODCALLTYPE Next(ULONG count,LPOLESTR* out,ULONG* fetched) override try {
        if(!out||(!fetched&&count!=1)) return E_POINTER; if(fetched) *fetched=0;
        ULONG n=static_cast<ULONG>(std::min<size_t>(count,items->size()-cursor)),done=0;
        try { for(;done<n;++done) out[done]=copy_string((*items)[cursor+done]); }
        catch(...) { for(ULONG i=0;i<done;++i) { CoTaskMemFree(out[i]); out[i]=nullptr; } throw; }
        cursor+=n; if(fetched) *fetched=n; return n==count?S_OK:S_FALSE;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE Skip(ULONG n) override { size_t k=std::min<size_t>(n,items->size()-cursor); cursor+=k; return k==n?S_OK:S_FALSE; }
    HRESULT STDMETHODCALLTYPE Reset() override { cursor=0; return S_OK; }
    HRESULT STDMETHODCALLTYPE Clone(IEnumString** out) override try {
        if(!out) return E_POINTER; *out=nullptr; *out=new StringEnumerator(items,cursor); return S_OK;
    } COM_CATCH
};
struct UnknownSnapshot {
    std::vector<IUnknown*> items;
    ~UnknownSnapshot() { for(auto* p:items) p->Release(); }
};
class UnknownEnumerator final: public IEnumUnknown,public RefCount {
    std::shared_ptr<const UnknownSnapshot> items; size_t cursor=0;
public:
    explicit UnknownEnumerator(std::shared_ptr<const UnknownSnapshot> data,size_t position=0):items(std::move(data)),cursor(position) {}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override {
        if(!out) return E_POINTER; *out=nullptr;
        if(iid!=IID_IUnknown&&iid!=IID_IEnumUnknown) return E_NOINTERFACE;
        *out=static_cast<IEnumUnknown*>(this); AddRef(); return S_OK;
    }
    COM_REFS
    HRESULT STDMETHODCALLTYPE Next(ULONG count,IUnknown** out,ULONG* fetched) override {
        if(!out||(!fetched&&count!=1)) return E_POINTER; if(fetched) *fetched=0;
        ULONG n=static_cast<ULONG>(std::min<size_t>(count,items->items.size()-cursor));
        for(ULONG i=0;i<n;++i) { out[i]=items->items[cursor+i]; out[i]->AddRef(); }
        cursor+=n; if(fetched) *fetched=n; return n==count?S_OK:S_FALSE;
    }
    HRESULT STDMETHODCALLTYPE Skip(ULONG n) override { size_t k=std::min<size_t>(n,items->items.size()-cursor); cursor+=k; return k==n?S_OK:S_FALSE; }
    HRESULT STDMETHODCALLTYPE Reset() override { cursor=0; return S_OK; }
    HRESULT STDMETHODCALLTYPE Clone(IEnumUnknown** out) override try {
        if(!out) return E_POINTER; *out=nullptr; *out=new UnknownEnumerator(items,cursor); return S_OK;
    } COM_CATCH
};

struct Group final: IOPCGroupStateMgt,IOPCItemMgt,IOPCSyncIO,RefCount {
    std::shared_ptr<State> state;
    std::wstring name;
    OPCHANDLE handle,client,next_item=1;
    DWORD rate,locale;
    BOOL active;
    LONG bias;
    FLOAT deadband;
    bool attached=true;
    std::map<OPCHANDLE,Binding> bindings;
    Group(std::shared_ptr<State> s,std::wstring n,OPCHANDLE h,OPCHANDLE c,DWORD r,DWORD l,BOOL a,LONG b,FLOAT d):
        state(std::move(s)),name(std::move(n)),handle(h),client(c),rate(r),locale(l),active(a),bias(b),deadband(d) {}
    HRESULT connected() const { return !attached||!state->alive||state->disconnected?RPC_E_DISCONNECTED:S_OK; }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override {
        ++state->stats.query_interface_calls;
        if(!out) return E_POINTER; *out=nullptr;
        if(iid==IID_IUnknown||iid==IID_IOPCGroupStateMgt) *out=static_cast<IOPCGroupStateMgt*>(this);
        else if(iid==IID_IOPCItemMgt) *out=static_cast<IOPCItemMgt*>(this);
        else if(iid==IID_IOPCSyncIO) *out=static_cast<IOPCSyncIO*>(this);
        else return E_NOINTERFACE; AddRef(); return S_OK;
    }
    COM_REFS
    HRESULT STDMETHODCALLTYPE GetState(DWORD* r,BOOL* a,LPWSTR* n,LONG* b,FLOAT* d,DWORD* l,OPCHANDLE* c,OPCHANDLE* h) override try {
        if(!r||!a||!n||!b||!d||!l||!c||!h) return E_POINTER; *n=nullptr;
        if(FAILED(connected())) return connected();
        *n=copy_string(name); *r=rate; *a=active; *b=bias; *d=deadband; *l=locale; *c=client; *h=handle; return S_OK;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE SetState(DWORD* requested,DWORD* revised,BOOL* a,LONG* b,FLOAT* d,DWORD* l,OPCHANDLE* c) override {
        if(!revised) return E_POINTER; *revised=rate;
        if(FAILED(connected())) return connected();
        if((d&&!valid_deadband(*d))||(l&&!valid_locale(*l))) return E_INVALIDARG;
        if(requested) rate=std::max<DWORD>(1,*requested);
        if(a) active=*a; if(b) bias=*b; if(d) deadband=*d; if(l) locale=*l; if(c) client=*c;
        *revised=rate; return requested&&*requested!=rate?OPC_S_UNSUPPORTEDRATE:S_OK;
    }
    HRESULT STDMETHODCALLTYPE SetName(LPCWSTR value) override try {
        if(!value||!*value) return E_INVALIDARG; if(FAILED(connected())) return connected();
        for(const auto& pair:state->groups) if(pair.second!=this&&pair.second->name==value) return OPC_E_DUPLICATENAME;
        name=value; return S_OK;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE CloneGroup(LPCWSTR,REFIID,LPUNKNOWN*) override;
    HRESULT items(DWORD count,OPCITEMDEF* defs,OPCITEMRESULT** results,HRESULT** errors,bool add) {
        if(!results||!errors) return E_POINTER; *results=nullptr; *errors=nullptr;
        if(!count||!defs) return E_INVALIDARG; if(FAILED(connected())) return connected();
        auto r=task_array<OPCITEMRESULT>(count); auto e=task_array<HRESULT>(count); HRESULT aggregate=S_OK;
        for(DWORD i=0;i<count;++i) {
            const auto& d=defs[i]; HRESULT hr=S_OK;
            auto item=state->items.end();
            if(!d.szItemID||!*d.szItemID) hr=OPC_E_INVALIDITEMID;
            else if(d.szAccessPath&&*d.szAccessPath) hr=OPC_E_UNKNOWNPATH;
            else if(!allowed_type(d.vtRequestedDataType)) hr=OPC_E_BADTYPE;
            else if(d.dwBlobSize&&!d.pBlob) hr=E_INVALIDARG;
            else { item=state->items.find(d.szItemID); if(item==state->items.end()) hr=OPC_E_UNKNOWNITEMID; }
            if(SUCCEEDED(hr)) {
                r.get()[i].vtCanonicalDataType=VT_R8; r.get()[i].dwAccessRights=item->second.rights;
                if(add) {
                    if(!next_item) hr=E_OUTOFMEMORY;
                    else { OPCHANDLE h=next_item++; bindings.emplace(h,Binding{d.szItemID,d.hClient,d.bActive,d.vtRequestedDataType}); r.get()[i].hServer=h; }
                }
            }
            e.get()[i]=hr; if(FAILED(hr)) aggregate=S_FALSE;
        }
        *results=r.release(); *errors=e.release(); return aggregate;
    }
    HRESULT STDMETHODCALLTYPE AddItems(DWORD n,OPCITEMDEF* d,OPCITEMRESULT** r,HRESULT** e) override try {
        ++state->stats.add_items_calls; return items(n,d,r,e,true);
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE ValidateItems(DWORD n,OPCITEMDEF* d,BOOL,OPCITEMRESULT** r,HRESULT** e) override try { return items(n,d,r,e,false); } COM_CATCH
    template<class Action> HRESULT each(DWORD n,OPCHANDLE* handles,HRESULT** errors,Action action) {
        if(!errors) return E_POINTER; *errors=nullptr;
        if(!n||!handles) return E_INVALIDARG; if(FAILED(connected())) return connected();
        auto e=task_array<HRESULT>(n); HRESULT result=S_OK;
        for(DWORD i=0;i<n;++i) {
            auto b=bindings.find(handles[i]); HRESULT hr=b==bindings.end()?OPC_E_INVALIDHANDLE:action(b,i);
            e.get()[i]=hr; if(FAILED(hr)) result=S_FALSE;
        }
        *errors=e.release(); return result;
    }
    HRESULT STDMETHODCALLTYPE RemoveItems(DWORD n,OPCHANDLE* h,HRESULT** e) override try {
        return each(n,h,e,[&](auto it,DWORD) { bindings.erase(it); return S_OK; });
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE SetActiveState(DWORD n,OPCHANDLE* h,BOOL a,HRESULT** e) override try {
        return each(n,h,e,[&](auto it,DWORD) { it->second.active=a; return S_OK; });
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE SetClientHandles(DWORD n,OPCHANDLE* h,OPCHANDLE* c,HRESULT** e) override try {
        if(!c) { if(e) *e=nullptr; return E_INVALIDARG; }
        return each(n,h,e,[&](auto it,DWORD i) { it->second.client=c[i]; return S_OK; });
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE SetDatatypes(DWORD n,OPCHANDLE* h,VARTYPE* t,HRESULT** e) override try {
        if(!t) { if(e) *e=nullptr; return E_INVALIDARG; }
        return each(n,h,e,[&](auto it,DWORD i) { if(!allowed_type(t[i])) return OPC_E_BADTYPE; it->second.requested=t[i]; return S_OK; });
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE CreateEnumerator(REFIID iid,LPUNKNOWN* out) override try {
        if(!out) return E_POINTER; *out=nullptr; if(FAILED(connected())) return connected();
        if(iid!=IID_IEnumOPCItemAttributes) return E_NOINTERFACE;
        auto snapshot=std::make_shared<std::vector<Attribute>>(); snapshot->reserve(bindings.size());
        for(const auto& b:bindings) snapshot->push_back(Attribute{b.second,b.first,state->items.at(b.second.id).rights});
        *out=static_cast<IEnumOPCItemAttributes*>(new AttributeEnumerator(snapshot)); return S_OK;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE Read(OPCDATASOURCE source,DWORD count,OPCHANDLE* handles,OPCITEMSTATE** values,HRESULT** errors) override try {
        ++state->stats.read_calls; state->stats.read_items+=count; Timer timer(state->stats.read_nanoseconds);
        if(!values||!errors) return E_POINTER; *values=nullptr; *errors=nullptr;
        if(!count||!handles||(source!=OPC_DS_CACHE&&source!=OPC_DS_DEVICE)) return E_INVALIDARG;
        if(FAILED(connected())) { state->stats.read_errors+=count; return connected(); }
        auto v=task_array<OPCITEMSTATE>(count); auto e=task_array<HRESULT>(count); HRESULT aggregate=S_OK;
        for(DWORD i=0;i<count;++i) {
            HRESULT hr=S_OK; auto b=bindings.find(handles[i]);
            if(b==bindings.end()) hr=OPC_E_INVALIDHANDLE;
            else {
                auto& r=v.get()[i]; const auto& item=state->items.at(b->second.id);
                r.hClient=b->second.client;
                if(!(item.rights&OPC_READABLE)) hr=OPC_E_BADRIGHTS;
                else {
                    r.ftTimeStamp=item.timestamp;
                    r.wQuality=item.bad_quality?OPC_QUALITY_BAD:item.quality;
                    if(source==OPC_DS_CACHE&&(!active||!b->second.active)) r.wQuality=OPC_QUALITY_OUT_OF_SERVICE;
                    VARTYPE type=b->second.requested==VT_EMPTY?VT_R8:b->second.requested;
                    hr=VariantChangeType(&r.vDataValue,const_cast<VARIANT*>(&item.value),0,type);
                    if(FAILED(hr)) hr=hr==DISP_E_OVERFLOW?OPC_E_RANGE:OPC_E_BADTYPE;
                    else if((r.wQuality&0xC0)!=OPC_QUALITY_GOOD) ++state->stats.bad_quality_items;
                }
            }
            e.get()[i]=hr;
            if(FAILED(hr)) { aggregate=S_FALSE; ++state->stats.read_errors; }
        }
        *values=v.release(); *errors=e.release(); return aggregate;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE Write(DWORD count,OPCHANDLE* handles,VARIANT* values,HRESULT** errors) override try {
        ++state->stats.write_calls; state->stats.write_items+=count; Timer timer(state->stats.write_nanoseconds);
        if(!errors) return E_POINTER; *errors=nullptr; if(!count||!handles||!values) return E_INVALIDARG;
        if(FAILED(connected())) { state->stats.write_errors+=count; return connected(); }
        auto e=task_array<HRESULT>(count); HRESULT aggregate=S_OK;
        for(DWORD i=0;i<count;++i) {
            HRESULT hr=S_OK; auto b=bindings.find(handles[i]);
            if(b==bindings.end()) hr=OPC_E_INVALIDHANDLE;
            else {
                auto& item=state->items.at(b->second.id);
                if(!(item.rights&OPC_WRITEABLE)||item.write_denied) hr=OPC_E_BADRIGHTS;
                else {
                    double value; hr=numeric_value(values[i],value);
                    if(SUCCEEDED(hr)) { item.value.dblVal=value; item.timestamp=state->clock; state->last_update=state->clock; }
                }
            }
            e.get()[i]=hr; if(FAILED(hr)) { aggregate=S_FALSE; ++state->stats.write_errors; }
        }
        *errors=e.release(); return aggregate;
    } COM_CATCH
};
HRESULT add_group(const std::shared_ptr<State>& state,LPCWSTR name,BOOL active,DWORD requested,OPCHANDLE client,LONG bias,FLOAT deadband,DWORD locale,OPCHANDLE* handle,DWORD* revised,REFIID iid,LPUNKNOWN* out) {
    if(!out||!handle||!revised) return E_POINTER; *out=nullptr; *handle=0; *revised=0;
    if(state->disconnected||!state->alive) return RPC_E_DISCONNECTED;
    if(!valid_deadband(deadband)||!valid_locale(locale)) return E_INVALIDARG;
    if(!state->next_group) return E_OUTOFMEMORY;
    OPCHANDLE h=state->next_group++;
    std::wstring actual=name&&*name?name:L"Group"+std::to_wstring(h);
    for(const auto& g:state->groups) if(g.second->name==actual) return OPC_E_DUPLICATENAME;
    auto group=std::make_unique<Group>(state,std::move(actual),h,client,std::max<DWORD>(1,requested),locale,active,bias,deadband);
    HRESULT hr=group->QueryInterface(iid,reinterpret_cast<void**>(out)); if(FAILED(hr)) return hr;
    try { state->groups.emplace(h,group.get()); }
    catch(...) { (*out)->Release(); *out=nullptr; throw; }
    group.release(); *handle=h; *revised=std::max<DWORD>(1,requested); return requested==*revised?S_OK:OPC_S_UNSUPPORTEDRATE;
}
HRESULT Group::CloneGroup(LPCWSTR new_name,REFIID iid,LPUNKNOWN* out) try {
    if(!out) return E_POINTER; *out=nullptr; if(FAILED(connected())) return connected();
    OPCHANDLE h; DWORD revised;
    HRESULT hr=add_group(state,new_name,FALSE,rate,client,bias,deadband,locale,&h,&revised,iid,out);
    if(FAILED(hr)) return hr;
    auto* clone=state->groups.at(h);
    try { clone->bindings=bindings; clone->next_item=next_item; }
    catch(...) { (*out)->Release(); *out=nullptr; state->groups.erase(h); clone->attached=false; clone->Release(); throw; }
    return S_OK;
} COM_CATCH

class Server final: public IOPCServer,public ISimulatorControl,public RefCount {
    std::shared_ptr<State> state=std::make_shared<State>();
public:
    ~Server() override {
        state->alive=false;
        for(auto& g:state->groups) { g.second->attached=false; g.second->Release(); }
        state->groups.clear();
    }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override {
        ++state->stats.query_interface_calls;
        if(!out) return E_POINTER; *out=nullptr;
        if(iid==IID_IUnknown||iid==IID_IOPCServer) *out=static_cast<IOPCServer*>(this);
        else if(iid==IID_ISimulatorControl) *out=static_cast<ISimulatorControl*>(this);
        else return E_NOINTERFACE; AddRef(); return S_OK;
    }
    COM_REFS
    HRESULT STDMETHODCALLTYPE AddGroup(LPCWSTR n,BOOL a,DWORD r,OPCHANDLE c,LONG* b,FLOAT* d,DWORD l,OPCHANDLE* h,DWORD* revised,REFIID iid,LPUNKNOWN* out) override try {
        return add_group(state,n,a,r,c,b?*b:0,d?*d:0,l,h,revised,iid,out);
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE GetErrorString(HRESULT error,LCID locale,LPWSTR* out) override try {
        if(!out) return E_POINTER; *out=nullptr; if(!valid_locale(locale)) return E_INVALIDARG;
        const wchar_t* text=nullptr;
        switch(error) {
            case OPC_E_INVALIDHANDLE: text=L"Unknown server handle"; break;
            case OPC_E_BADTYPE: text=L"Unsupported or unconvertible data type"; break;
            case OPC_E_BADRIGHTS: text=L"Item access rights deny this operation"; break;
            case OPC_E_UNKNOWNITEMID: text=L"Unknown item identifier"; break;
            case OPC_E_INVALIDITEMID: text=L"Empty item identifier"; break;
            case OPC_E_UNKNOWNPATH: text=L"Only the empty access path is supported"; break;
            case OPC_E_RANGE: text=L"Numeric value outside representable range"; break;
            case OPC_E_DUPLICATENAME: text=L"Group name already exists"; break;
            case OPC_E_NOTFOUND: text=L"Group not found"; break;
            case OPC_S_UNSUPPORTEDRATE: text=L"Update rate revised to supported rate"; break;
            case OPC_S_INUSE: text=L"Group still has external references"; break;
        }
        if(text) { *out=copy_string(text); return S_OK; }
        wchar_t* system=nullptr;
        DWORD n=FormatMessageW(FORMAT_MESSAGE_ALLOCATE_BUFFER|FORMAT_MESSAGE_FROM_SYSTEM|FORMAT_MESSAGE_IGNORE_INSERTS,nullptr,static_cast<DWORD>(error),0,reinterpret_cast<LPWSTR>(&system),0,nullptr);
        if(!n) return E_INVALIDARG;
        try { *out=copy_string(system); } catch(...) { LocalFree(system); throw; }
        LocalFree(system); return S_OK;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE GetGroupByName(LPCWSTR name,REFIID iid,LPUNKNOWN* out) override {
        if(!out) return E_POINTER; *out=nullptr; if(!name) return E_INVALIDARG;
        if(state->disconnected) return RPC_E_DISCONNECTED;
        for(const auto& g:state->groups) if(g.second->name==name) return g.second->QueryInterface(iid,reinterpret_cast<void**>(out));
        return OPC_E_NOTFOUND;
    }
    HRESULT STDMETHODCALLTYPE GetStatus(OPCSERVERSTATUS** out) override try {
        if(!out) return E_POINTER; *out=nullptr;
        auto s=task_array<OPCSERVERSTATUS>(1);
        s->ftStartTime=state->start; s->ftCurrentTime=state->clock; s->ftLastUpdateTime=state->last_update;
        s->dwServerState=state->disconnected?OPC_STATUS_COMM_FAULT:(state->configured?OPC_STATUS_RUNNING:OPC_STATUS_NOCONFIG);
        s->dwGroupCount=static_cast<DWORD>(state->groups.size()); s->dwBandWidth=0xffffffff;
        s->wMajorVersion=1; s->szVendorInfo=copy_string(L"AuTHRity isolated local synchronous COM simulator");
        *out=s.release(); return S_OK;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE RemoveGroup(OPCHANDLE handle,BOOL force) override {
        if(state->disconnected) return RPC_E_DISCONNECTED;
        auto it=state->groups.find(handle); if(it==state->groups.end()) return OPC_E_INVALIDHANDLE;
        Group* g=it->second; if(!force&&g->references>1) return OPC_S_INUSE;
        state->groups.erase(it); g->attached=false; g->Release(); return S_OK;
    }
    HRESULT STDMETHODCALLTYPE CreateGroupEnumerator(OPCENUMSCOPE scope,REFIID iid,LPUNKNOWN* out) override try {
        if(!out) return E_POINTER; *out=nullptr;
        if(state->disconnected) return RPC_E_DISCONNECTED;
        if(scope<OPC_ENUM_PRIVATE_CONNECTIONS||scope>OPC_ENUM_ALL) return E_INVALIDARG;
        bool is_public=scope==OPC_ENUM_PUBLIC_CONNECTIONS||scope==OPC_ENUM_PUBLIC;
        if(iid==IID_IEnumString) {
            auto names=std::make_shared<std::vector<std::wstring>>();
            if(!is_public) for(const auto& g:state->groups) names->push_back(g.second->name);
            *out=static_cast<IEnumString*>(new StringEnumerator(names)); return S_OK;
        }
        if(iid==IID_IEnumUnknown) {
            auto objects=std::make_shared<UnknownSnapshot>();
            if(!is_public) {
                objects->items.reserve(state->groups.size());
                for(const auto& g:state->groups) { objects->items.push_back(static_cast<IOPCGroupStateMgt*>(g.second)); g.second->AddRef(); }
            }
            *out=static_cast<IEnumUnknown*>(new UnknownEnumerator(objects)); return S_OK;
        }
        return E_NOINTERFACE;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE Configure(DWORD count,const SimItem* items,FILETIME clock) override try {
        if(!count||!items) return E_INVALIDARG;
        if(state->configured||!state->groups.empty()) return HRESULT_FROM_WIN32(ERROR_ALREADY_INITIALIZED);
        std::map<std::wstring,Item,std::less<>> configured;
        for(DWORD i=0;i<count;++i) {
            const auto& x=items[i]; if(!x.id||!*x.id||!std::isfinite(x.value)) return E_INVALIDARG;
            DWORD rights=(x.readable?OPC_READABLE:0)|(x.writable?OPC_WRITEABLE:0);
            if(!configured.emplace(x.id,Item{x.value,rights,x.quality,x.timestamp}).second) return OPC_E_DUPLICATENAME;
        }
        state->items.swap(configured); state->configured=true; state->clock=clock; state->start=clock; state->last_update=clock; return S_OK;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE Update(DWORD count,const SimUpdate* updates,FILETIME clock) override {
        ++state->stats.update_calls;
        if(!state->configured||!count||!updates) return E_INVALIDARG;
        // Validate the whole management update before committing any physical value.
        for(DWORD i=0;i<count;++i) {
            if(!updates[i].id||!std::isfinite(updates[i].value)) return E_INVALIDARG;
            if(state->items.find(updates[i].id)==state->items.end()) return OPC_E_UNKNOWNITEMID;
        }
        for(DWORD i=0;i<count;++i) {
            auto& item=state->items.find(updates[i].id)->second;
            item.value.dblVal=updates[i].value; item.quality=updates[i].quality; item.timestamp=updates[i].timestamp;
        }
        state->clock=clock; state->last_update=clock; return S_OK;
    }
    HRESULT STDMETHODCALLTYPE SetClock(FILETIME clock) override { state->clock=clock; return S_OK; }
    HRESULT STDMETHODCALLTYPE SetFault(SimFault fault,BOOL enabled,DWORD count,LPCWSTR const* ids) override {
        ++state->stats.fault_calls;
        if(fault<SIM_BAD_QUALITY||fault>SIM_WRITE_DENIED||(count&&!ids)) return E_INVALIDARG;
        if(fault==SIM_DISCONNECT) { if(count) return E_INVALIDARG; state->disconnected=enabled!=FALSE; return S_OK; }
        for(DWORD i=0;i<count;++i) if(!ids[i]||state->items.find(ids[i])==state->items.end()) return OPC_E_UNKNOWNITEMID;
        auto apply=[&](Item& item) { if(fault==SIM_BAD_QUALITY) item.bad_quality=enabled!=FALSE; else item.write_denied=enabled!=FALSE; };
        if(!count) for(auto& item:state->items) apply(item.second);
        else for(DWORD i=0;i<count;++i) apply(state->items.find(ids[i])->second);
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetStatistics(SimStats* out) override { if(!out) return E_POINTER; *out=state->stats; return S_OK; }
};
class Factory final: public IClassFactory,public RefCount {
public:
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override {
        if(!out) return E_POINTER; *out=nullptr;
        if(iid!=IID_IUnknown&&iid!=IID_IClassFactory) return E_NOINTERFACE;
        *out=static_cast<IClassFactory*>(this); AddRef(); return S_OK;
    }
    COM_REFS
    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer,REFIID iid,void** out) override try {
        if(!out) return E_POINTER; *out=nullptr; if(outer) return CLASS_E_NOAGGREGATION;
        auto server=std::make_unique<Server>(); HRESULT hr=server->QueryInterface(iid,out);
        if(SUCCEEDED(hr)) server.release()->Release(); return hr;
    } COM_CATCH
    HRESULT STDMETHODCALLTYPE LockServer(BOOL lock) override {
        if(lock) ++server_locks; else if(server_locks>0) --server_locks; else return E_UNEXPECTED;
        return S_OK;
    }
};
HRESULT registry_string(const std::wstring& path,const wchar_t* name,const std::wstring& value) {
    HKEY key=nullptr; LONG error=RegCreateKeyExW(HKEY_CURRENT_USER,path.c_str(),0,nullptr,0,KEY_SET_VALUE,nullptr,&key,nullptr);
    if(error!=ERROR_SUCCESS) return HRESULT_FROM_WIN32(error);
    error=RegSetValueExW(key,name,0,REG_SZ,reinterpret_cast<const BYTE*>(value.c_str()),static_cast<DWORD>((value.size()+1)*sizeof(wchar_t)));
    RegCloseKey(key); return HRESULT_FROM_WIN32(error);
}
}
extern "C" BOOL WINAPI DllMain(HINSTANCE instance,DWORD reason,LPVOID) {
    if(reason==DLL_PROCESS_ATTACH) { module_handle=instance; DisableThreadLibraryCalls(instance); } return TRUE;
}
extern "C" HRESULT WINAPI DllGetClassObject(REFCLSID clsid,REFIID iid,void** out) try {
    if(!out) return E_POINTER; *out=nullptr; if(clsid!=CLSID_AuTHRityDaSimulator) return CLASS_E_CLASSNOTAVAILABLE;
    auto factory=std::make_unique<Factory>(); HRESULT hr=factory->QueryInterface(iid,out);
    if(SUCCEEDED(hr)) factory.release()->Release(); return hr;
} COM_CATCH
extern "C" HRESULT WINAPI DllCanUnloadNow() { return live_objects==0&&server_locks==0?S_OK:S_FALSE; }
extern "C" HRESULT WINAPI DllRegisterServer() try {
    wchar_t path[32768]; DWORD count=GetModuleFileNameW(module_handle,path,32768);
    if(!count||count==32768) return HRESULT_FROM_WIN32(count?ERROR_INSUFFICIENT_BUFFER:GetLastError());
    const std::wstring clsid=std::wstring(L"Software\\Classes\\CLSID\\")+SIM_CLSID_TEXT;
    HRESULT hr=registry_string(clsid,nullptr,L"AuTHRity isolated local DA simulator"); if(FAILED(hr)) return hr;
    hr=registry_string(clsid+L"\\InprocServer32",nullptr,path); if(FAILED(hr)) return hr;
    hr=registry_string(clsid+L"\\InprocServer32",L"ThreadingModel",L"Apartment"); if(FAILED(hr)) return hr;
    hr=registry_string(clsid+L"\\ProgID",nullptr,SIM_PROGID); if(FAILED(hr)) return hr;
    return registry_string(std::wstring(L"Software\\Classes\\")+SIM_PROGID+L"\\CLSID",nullptr,SIM_CLSID_TEXT);
} COM_CATCH
extern "C" HRESULT WINAPI DllUnregisterServer() try {
    const std::wstring clsid=std::wstring(L"Software\\Classes\\CLSID\\")+SIM_CLSID_TEXT;
    LONG a=RegDeleteTreeW(HKEY_CURRENT_USER,clsid.c_str());
    LONG b=RegDeleteTreeW(HKEY_CURRENT_USER,(std::wstring(L"Software\\Classes\\")+SIM_PROGID).c_str());
    if(a!=ERROR_SUCCESS&&a!=ERROR_FILE_NOT_FOUND) return HRESULT_FROM_WIN32(a);
    if(b!=ERROR_SUCCESS&&b!=ERROR_FILE_NOT_FOUND) return HRESULT_FROM_WIN32(b); return S_OK;
} COM_CATCH
