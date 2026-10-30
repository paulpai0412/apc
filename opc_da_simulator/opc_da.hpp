#pragma once
// ABI subset derived from OPC Foundation opcda.idl (MIT); see LICENSE.OPC.
// This is a local, synchronous custom-interface profile, not a full DA certification.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <objbase.h>
#include <oleauto.h>
#include <cstdint>

using OPCHANDLE = DWORD;
enum OPCDATASOURCE { OPC_DS_CACHE = 1, OPC_DS_DEVICE = 2 };
enum OPCEUTYPE { OPC_NOENUM = 0, OPC_ANALOG = 1, OPC_ENUMERATED = 2 };
enum OPCSERVERSTATE { OPC_STATUS_RUNNING = 1, OPC_STATUS_FAILED, OPC_STATUS_NOCONFIG,
    OPC_STATUS_SUSPENDED, OPC_STATUS_TEST, OPC_STATUS_COMM_FAULT };
enum OPCENUMSCOPE { OPC_ENUM_PRIVATE_CONNECTIONS = 1, OPC_ENUM_PUBLIC_CONNECTIONS,
    OPC_ENUM_ALL_CONNECTIONS, OPC_ENUM_PRIVATE, OPC_ENUM_PUBLIC, OPC_ENUM_ALL };
struct OPCITEMSTATE { OPCHANDLE hClient; FILETIME ftTimeStamp; WORD wQuality;
    WORD wReserved; VARIANT vDataValue; };
struct OPCSERVERSTATUS { FILETIME ftStartTime, ftCurrentTime, ftLastUpdateTime;
    OPCSERVERSTATE dwServerState; DWORD dwGroupCount, dwBandWidth;
    WORD wMajorVersion, wMinorVersion, wBuildNumber, wReserved; LPWSTR szVendorInfo; };
struct OPCITEMDEF { LPWSTR szAccessPath, szItemID; BOOL bActive; OPCHANDLE hClient;
    DWORD dwBlobSize; BYTE* pBlob; VARTYPE vtRequestedDataType; WORD wReserved; };
struct OPCITEMRESULT { OPCHANDLE hServer; VARTYPE vtCanonicalDataType; WORD wReserved;
    DWORD dwAccessRights, dwBlobSize; BYTE* pBlob; };
struct OPCITEMATTRIBUTES { LPWSTR szAccessPath, szItemID; BOOL bActive;
    OPCHANDLE hClient, hServer; DWORD dwAccessRights, dwBlobSize; BYTE* pBlob;
    VARTYPE vtRequestedDataType, vtCanonicalDataType; OPCEUTYPE dwEUType; VARIANT vEUInfo; };

#define DA_IID(name, first) inline constexpr GUID name = {first,0x011e,0x11d0,{0x96,0x75,0x00,0x20,0xaf,0xd8,0xad,0xb3}}
DA_IID(IID_IOPCServer, 0x39c13a4d);
DA_IID(IID_IOPCGroupStateMgt, 0x39c13a50);
DA_IID(IID_IOPCSyncIO, 0x39c13a52);
DA_IID(IID_IOPCItemMgt, 0x39c13a54);
DA_IID(IID_IEnumOPCItemAttributes, 0x39c13a55);
#undef DA_IID
inline constexpr GUID CLSID_AuTHRityDaSimulator = {0xbce86c29,0x5dcb,0x43d8,{0xa6,0xa0,0x46,0x99,0xb5,0x03,0xa0,0xb8}};
inline constexpr GUID IID_ISimulatorControl = {0x638508bc,0x0fa2,0x493d,{0x99,0x1f,0x66,0x4c,0x2c,0x24,0x04,0x39}};
inline constexpr wchar_t SIM_PROGID[] = L"AuTHRity.LocalDaSimulator.1";
inline constexpr wchar_t SIM_CLSID_TEXT[] = L"{BCE86C29-5DCB-43D8-A6A0-4699B503A0B8}";
inline constexpr HRESULT OPC_E_INVALIDHANDLE = static_cast<HRESULT>(0xC0040001u);
inline constexpr HRESULT OPC_E_BADTYPE = static_cast<HRESULT>(0xC0040004u);
inline constexpr HRESULT OPC_E_BADRIGHTS = static_cast<HRESULT>(0xC0040006u);
inline constexpr HRESULT OPC_E_UNKNOWNITEMID = static_cast<HRESULT>(0xC0040007u);
inline constexpr HRESULT OPC_E_INVALIDITEMID = static_cast<HRESULT>(0xC0040008u);
inline constexpr HRESULT OPC_E_UNKNOWNPATH = static_cast<HRESULT>(0xC004000Au);
inline constexpr HRESULT OPC_E_RANGE = static_cast<HRESULT>(0xC004000Bu);
inline constexpr HRESULT OPC_E_DUPLICATENAME = static_cast<HRESULT>(0xC004000Cu);
inline constexpr HRESULT OPC_E_NOTFOUND = static_cast<HRESULT>(0xC0040011u);
inline constexpr HRESULT OPC_S_UNSUPPORTEDRATE = 0x0004000D;
inline constexpr HRESULT OPC_S_INUSE = 0x0004000F;
inline constexpr DWORD OPC_READABLE = 1, OPC_WRITEABLE = 2;
inline constexpr WORD OPC_QUALITY_GOOD = 0xC0, OPC_QUALITY_BAD = 0,
    OPC_QUALITY_OUT_OF_SERVICE = 0x1C;

struct IOPCServer : IUnknown {
    virtual HRESULT STDMETHODCALLTYPE AddGroup(LPCWSTR,BOOL,DWORD,OPCHANDLE,LONG*,FLOAT*,DWORD,OPCHANDLE*,DWORD*,REFIID,LPUNKNOWN*) = 0;
    virtual HRESULT STDMETHODCALLTYPE GetErrorString(HRESULT,LCID,LPWSTR*) = 0;
    virtual HRESULT STDMETHODCALLTYPE GetGroupByName(LPCWSTR,REFIID,LPUNKNOWN*) = 0;
    virtual HRESULT STDMETHODCALLTYPE GetStatus(OPCSERVERSTATUS**) = 0;
    virtual HRESULT STDMETHODCALLTYPE RemoveGroup(OPCHANDLE,BOOL) = 0;
    virtual HRESULT STDMETHODCALLTYPE CreateGroupEnumerator(OPCENUMSCOPE,REFIID,LPUNKNOWN*) = 0;
};
struct IOPCGroupStateMgt : IUnknown {
    virtual HRESULT STDMETHODCALLTYPE GetState(DWORD*,BOOL*,LPWSTR*,LONG*,FLOAT*,DWORD*,OPCHANDLE*,OPCHANDLE*) = 0;
    virtual HRESULT STDMETHODCALLTYPE SetState(DWORD*,DWORD*,BOOL*,LONG*,FLOAT*,DWORD*,OPCHANDLE*) = 0;
    virtual HRESULT STDMETHODCALLTYPE SetName(LPCWSTR) = 0;
    virtual HRESULT STDMETHODCALLTYPE CloneGroup(LPCWSTR,REFIID,LPUNKNOWN*) = 0;
};
struct IOPCItemMgt : IUnknown {
    virtual HRESULT STDMETHODCALLTYPE AddItems(DWORD,OPCITEMDEF*,OPCITEMRESULT**,HRESULT**) = 0;
    virtual HRESULT STDMETHODCALLTYPE ValidateItems(DWORD,OPCITEMDEF*,BOOL,OPCITEMRESULT**,HRESULT**) = 0;
    virtual HRESULT STDMETHODCALLTYPE RemoveItems(DWORD,OPCHANDLE*,HRESULT**) = 0;
    virtual HRESULT STDMETHODCALLTYPE SetActiveState(DWORD,OPCHANDLE*,BOOL,HRESULT**) = 0;
    virtual HRESULT STDMETHODCALLTYPE SetClientHandles(DWORD,OPCHANDLE*,OPCHANDLE*,HRESULT**) = 0;
    virtual HRESULT STDMETHODCALLTYPE SetDatatypes(DWORD,OPCHANDLE*,VARTYPE*,HRESULT**) = 0;
    virtual HRESULT STDMETHODCALLTYPE CreateEnumerator(REFIID,LPUNKNOWN*) = 0;
};
struct IOPCSyncIO : IUnknown {
    virtual HRESULT STDMETHODCALLTYPE Read(OPCDATASOURCE,DWORD,OPCHANDLE*,OPCITEMSTATE**,HRESULT**) = 0;
    virtual HRESULT STDMETHODCALLTYPE Write(DWORD,OPCHANDLE*,VARIANT*,HRESULT**) = 0;
};
struct IEnumOPCItemAttributes : IUnknown {
    virtual HRESULT STDMETHODCALLTYPE Next(ULONG,OPCITEMATTRIBUTES**,ULONG*) = 0;
    virtual HRESULT STDMETHODCALLTYPE Skip(ULONG) = 0;
    virtual HRESULT STDMETHODCALLTYPE Reset() = 0;
    virtual HRESULT STDMETHODCALLTYPE Clone(IEnumOPCItemAttributes**) = 0;
};

// Explicit simulator-only management plane. No transport read/write is implemented here.
// All IDs and buffers are borrowed for the duration of a same-apartment COM call.
struct SimItem { LPCWSTR id; DOUBLE value; BOOL readable, writable; WORD quality; FILETIME timestamp; };
struct SimUpdate { LPCWSTR id; DOUBLE value; WORD quality; FILETIME timestamp; };
enum SimFault : DWORD { SIM_BAD_QUALITY = 1, SIM_DISCONNECT = 2, SIM_WRITE_DENIED = 3 };
struct SimStats {
    ULONGLONG query_interface_calls, add_items_calls, read_calls, write_calls;
    ULONGLONG read_items, write_items, read_errors, write_errors, bad_quality_items;
    ULONGLONG update_calls, fault_calls, read_nanoseconds, write_nanoseconds;
};
struct ISimulatorControl : IUnknown {
    virtual HRESULT STDMETHODCALLTYPE Configure(DWORD,const SimItem*,FILETIME) = 0;
    virtual HRESULT STDMETHODCALLTYPE Update(DWORD,const SimUpdate*,FILETIME) = 0;
    virtual HRESULT STDMETHODCALLTYPE SetClock(FILETIME) = 0;
    virtual HRESULT STDMETHODCALLTYPE SetFault(SimFault,BOOL,DWORD,LPCWSTR const*) = 0;
    virtual HRESULT STDMETHODCALLTYPE GetStatistics(SimStats*) = 0;
};

inline FILETIME unix_ms_to_filetime(std::int64_t ms) {
    ULARGE_INTEGER t; t.QuadPart = (static_cast<ULONGLONG>(ms) + 11644473600000ULL) * 10000ULL;
    return FILETIME{t.LowPart,t.HighPart};
}
inline std::int64_t filetime_to_unix_ms(FILETIME ft) {
    ULARGE_INTEGER t; t.LowPart=ft.dwLowDateTime; t.HighPart=ft.dwHighDateTime;
    return static_cast<std::int64_t>(t.QuadPart/10000ULL)-11644473600000LL;
}
