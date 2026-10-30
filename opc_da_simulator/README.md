# Isolated synchronous OPC DA interoperability simulator

This directory implements a **real Windows COM in-process server** and a separate
console client using the OPC DA custom-interface ABI. It is an isolated transport
validation fixture, not an ABB device emulator, production DCS integration,
certified DA server, remote DCOM server, or replacement for commissioning.

No network listener, production credentials, host machine-wide registration, SDK
installation key, downloaded executable source generator, or commercial library
is needed. Windows DLL registration is private and per-user:

- ProgID: `AuTHRity.LocalDaSimulator.1`
- CLSID: `{BCE86C29-5DCB-43D8-A6A0-4699B503A0B8}`
- Registration root: `HKCU\Software\Classes`
- Threading model: `Apartment`, 64-bit, same-process/same-apartment only

The bridge obtains the CLSID from the registry, activates it with
`CoCreateInstance(..., CLSCTX_INPROC_SERVER, IID_IOPCServer, ...)`, creates a DA
group, adds the configured items through `IOPCItemMgt::AddItems`, obtains
`IOPCSyncIO` using `QueryInterface`, and uses actual `Read`/`Write` vtable calls
for **every** process-input read, command write, and command readback. The bridge
neither links server implementation code nor accesses server memory. The server
owns the values, variants, qualities, access rights, FILETIMEs, and error counters.

## Supported profile and deliberate boundaries

Every method of these advertised interfaces is implemented:

- `IOPCServer`: AddGroup, GetErrorString, GetGroupByName, GetStatus, RemoveGroup,
  CreateGroupEnumerator.
- `IOPCGroupStateMgt`: GetState, SetState, SetName, CloneGroup.
- `IOPCItemMgt`: AddItems, ValidateItems, RemoveItems, SetActiveState,
  SetClientHandles, SetDatatypes, CreateEnumerator.
- `IOPCSyncIO`: synchronous Read and Write.
- `IEnumOPCItemAttributes`, `IEnumString`, `IEnumUnknown`: Next, Skip, Reset, Clone.
- `IUnknown` and activation through `IClassFactory`, including aggregation
  rejection and DLL unload reference tracking.

Unsupported interfaces return `E_NOINTERFACE`; no advertised method is an
`E_NOTIMPL` stub. There is no async subscription/data callback, public-group
creation, address-space browsing, DA 3 ItemIO/VQT, automation `IDispatch`,
proxy/stub marshaling, out-of-process service, remote DCOM, or component-category
registration claiming a complete DA version. Public-group enumerations are
empty. Clients must know item IDs from the explicit initialization configuration.
English error text and LCIDs 0, 0x409, system/user-default are supported.

Canonical items are finite scalar `VT_R8`. Supported requested conversions are
`VT_EMPTY` (canonical), `R8`, `R4`, signed/unsigned integer widths, `INT`, `UINT`,
`BOOL`, `BSTR`, `CY`, and `DECIMAL`, through Windows `VariantChangeType`.
Arrays, objects, by-reference and unsupported variants are rejected. Conversion
failure/range overflow reports item errors; outputs use `CoTaskMemAlloc`, and
clients free returned variants using `VariantClear`. Items support independently
configured readable and writable rights. Unknown IDs, invalid handles, empty IDs,
nonempty access paths, duplicate names, invalid deadbands/locales, and denied
operations return concrete HRESULTs.

The simulator management clock is deterministic, not wall-clock paced. Device
and cache read the current simulation snapshot. Cache reads of an inactive group
or item return `OPC_QUALITY_OUT_OF_SERVICE`; device reads are independent of
active state. Group update rates (minimum 1 ms) and deadbands are configuration
metadata because this profile has no asynchronous sampling or notification
scheduler. Cloned groups begin inactive. Group removal with outstanding external
references returns `OPC_S_INUSE` unless forced; forced removal invalidates the
old group's operations. Enumerators hold stable snapshots and proper COM refs.

`ISimulatorControl` is explicitly **not OPC DA**. It configures the simulated
physical device, publishes plant-generated input values/timestamps, advances the
simulation clock, controls faults, and returns counters. This is the management
plane analogous to the plant behind an ordinary DA server; `op:update` is not
counted or presented as a DA write. Read-only physical measurements must use
`update`, not a command write. This custom local interface is intentionally not
an authentication/security boundary and must not be exposed outside isolation.

## Build

Dependencies: a 64-bit MinGW-w64 C++17 compiler, GNU make, and for Linux execution
Wine64. All C++ runtime libraries are statically linked. Windows APIs are linked
from `ole32`, `oleaut32`, `advapi32`, and `uuid`. No JSON package is required:
`json.hpp` is an original strict RFC 8259 parser/serializer with duplicate-key
rejection, Unicode/surrogate checking, finite-number validation, a 32-level
nesting limit, and an 8 MiB JSONL input-record limit.

From the repository root, cross-compile on Linux or in an MSYS2 MinGW64 shell:

```sh
make -C opc_da_simulator CXX=x86_64-w64-mingw32-g++
```

In an MSYS2 MINGW64 shell where the compiler is named `g++`, use:

```sh
make -C opc_da_simulator CXX=g++
```

Outputs are `opc_da_simulator/build/authrity_opc_da.dll`,
`opc_da_bridge.exe`, and `opc_da_tests.exe`. Override `CXX`, `CPPFLAGS`,
`CXXFLAGS`, or `LDFLAGS` on the make command line for an extracted local toolchain.
Builds do not register anything or run tests.

On 64-bit Windows (PowerShell, no elevation, paths relative to repo root):

```powershell
& "$env:WINDIR\System32\regsvr32.exe" /s "$PWD\opc_da_simulator\build\authrity_opc_da.dll"
& .\opc_da_simulator\build\opc_da_tests.exe
Get-Content .\opc_da_simulator\fault_scenario.jsonl | & .\opc_da_simulator\build\opc_da_bridge.exe
& "$env:WINDIR\System32\regsvr32.exe" /u /s "$PWD\opc_da_simulator\build\authrity_opc_da.dll"
```

For isolated Linux execution, these are **fish** commands from the repo root:

```fish
set -gx WINEPREFIX (mktemp -d /tmp/authrity-da-wine.XXXXXX)
set -gx WINEARCH win64
set -gx WINEDEBUG -all
wineboot --init
set dll (winepath -w "$PWD/opc_da_simulator/build/authrity_opc_da.dll")
wine regsvr32 /s "$dll"
# Wine 10 maps HKCR to the virtual machine hive, without the Windows per-user merge.
# These two copies are ONLY inside this dedicated disposable WINEPREFIX.
wine reg copy 'HKCU\Software\Classes\CLSID\{BCE86C29-5DCB-43D8-A6A0-4699B503A0B8}' 'HKLM\Software\Classes\CLSID\{BCE86C29-5DCB-43D8-A6A0-4699B503A0B8}' /s /f
wine reg copy 'HKCU\Software\Classes\AuTHRity.LocalDaSimulator.1' 'HKLM\Software\Classes\AuTHRity.LocalDaSimulator.1' /s /f
wine "$PWD/opc_da_simulator/build/opc_da_tests.exe"
wine "$PWD/opc_da_simulator/build/opc_da_bridge.exe" < opc_da_simulator/fault_scenario.jsonl
```

If Wine and MinGW are extracted beneath a private toolchain directory, point
`CXX` and `wine` at those executables and set `LD_LIBRARY_PATH` for their extracted
host libraries; keep the same isolated `WINEPREFIX`. Do not use the default Wine
prefix or register this simulator in a production user's profile.

The Wine 10 registry compatibility step is based on the observed HKCU registration
being absent from HKCR and Wine's [registry root implementation](https://github.com/wine-mirror/wine/blob/wine-10.0/dlls/kernelbase/registry.c).
It does not change the Windows server's per-user registration code or bypass
COM activation. Never run those hive-copy commands against native Windows or
a shared/default Wine prefix. When reusing this isolated prefix, remove both
private mirrored keys as well as calling `DllUnregisterServer`; discarding the
dedicated prefix removes all of its virtual registrations.

The tested extracted Ubuntu Wine package also required its NLS directory under
`usr/share/wine/wine/nls` (a local `wine -> .` symlink), the packaged x64
`zlib1.dll` beside the Wine x64 DLLs, and those runtime DLLs installed into the
prefix's `drive_c/windows/system32`. These are package-relocation dependencies,
not alternate simulator implementations. All native tests still use real
`CoCreateInstance`, `QueryInterface`, and synchronous DA calls.

Native tests exercise actual COM activation/QI identity, private groups, all
advertised group and item operations, all three enumerator types and cloning,
scalar conversion, invalid handles/types, read/write permissions, timestamps,
cache/device behavior, real bad quality, write denial, disconnection, and
counter consistency. JSON tests cover Unicode, escaping, invalid grammars,
duplicate keys, nonfinite input and round-trip precision. Tests are source, not
claims that a particular toolchain or machine has passed them.

## JSONL process contract

Start one long-lived `opc_da_bridge.exe`, with piped stdin/stdout. There is no
unsolicited stdout banner: the first successful `init` response is the readiness
handshake. Each input line produces exactly one flushed output line, in order.
The campaign starts Wine directly, not a shell, and inherits the isolated prefix:

```text
mpc_campaign closed-loop ... --transport opc-da \
  --bridge /path/to/wine --bridge-arg /absolute/path/to/opc_da_bridge.exe
```

The Rust runtime uses arbitrary indexed IDs (`CV:i`, `MV:i`, `DV:i`, `Target:i`,
`Command:i`) from its current configuration; the DLL contains none of these names.
All array results preserve request order. Numeric HRESULTs are signed 32-bit
values. `ok:true` means the COM call returned success, **not** that every item
succeeded or had good quality. Consumers must check each HRESULT, quality and
readback; `S_FALSE` and bad quality must not be silently accepted as control data.

Initialize once:

```json
{"op":"init","timestamp_ms":1767225600000,"items":[{"id":"any measurement","value":1.5,"writable":false,"readable":true,"quality":192},{"id":"any command","value":0,"writable":true}]}
```

`readable` defaults true, `quality` defaults 192, and `writable` is required.
Item IDs must be nonempty UTF-8 without NUL, at most 32767 bytes. There must be
1..100000 items per data operation. Timestamps are integer Unix milliseconds in
1970..9999, exactly representable in JSON's finite-double number profile.

Physical publication, command write and process read:

```json
{"op":"update","timestamp_ms":1767225660000,"items":[{"id":"any measurement","value":2.5,"quality":192,"timestamp_ms":1767225659990}]}
{"op":"write","timestamp_ms":1767225660000,"items":[{"id":"any command","value":0.25}]}
{"op":"read","items":["any measurement","any command"]}
```

The `update` item timestamp overrides the request clock when supplied, supporting
independent sensor acquisition times. `write` uses `ISimulatorControl::SetClock`
then **IOPCSyncIO::Write**; successful writes get that timestamp. `read` uses
**IOPCSyncIO::Read(OPC_DS_DEVICE)**. A successful read response contains
`values`, `quality`, `timestamp_ms`, and `hresult` arrays, plus `call_hresult` and
`stats`. Failed items have `null` values, quality 0, timestamp 0, and the actual
item HRESULT. Bad quality alone retains the real numeric value and a successful
item HRESULT. A write returns an item `hresult` array and `call_hresult`.

Fault management:

```json
{"op":"fault","kind":"bad_quality","enabled":true,"items":["any measurement"]}
{"op":"fault","kind":"write_denied","enabled":true,"items":["any command"]}
{"op":"fault","kind":"disconnect","enabled":true}
```

- `bad_quality`: the server's actual DA item state returns quality `0x00` while
  retaining its value/time and successful read HRESULT. It persists across
  physical updates until explicitly disabled.
- `write_denied`: actual `IOPCSyncIO::Write` returns `OPC_E_BADRIGHTS`
  (`0xC0040006`, signed `-1073479674`) for the selected items. No value changes.
- `disconnect`: actual DA group operations fail with `RPC_E_DISCONNECTED`
  (`0x80010108`, signed `-2147417848`), and GetStatus reports communication fault.
  This is an explicit server-generated disconnection fault, not a claim of a
  physical network break, COM process crash, or remote transport failure.

For bad quality/write denial, omitting `items` targets the entire registry; an
explicit empty list is rejected. Disconnect is server-wide and rejects an item
list. Setting `enabled:false` restores normal operation. The management interface
remains available during faults so an isolated test can recover. A COM-level
failure returns `{"ok":false,"hresult":...,"error":...,"stats":...}`; it is not
rewritten as a successful read. `fault_scenario.jsonl` provides an input-only
example exercising recovery and clean shutdown, not precomputed test results.

Every response after initialization includes counters obtained from the server:
`query_interface_calls`, `add_items_calls`, `read_calls`, `write_calls`,
`read_items`, `write_items`, `read_errors`, `write_errors`, `bad_quality_items`,
`update_calls`, `fault_calls`, `read_nanoseconds`, `write_nanoseconds`. Read/write
timing is measured inside the actual COM methods with QueryPerformanceCounter;
it excludes JSON, pipe, Wine startup and bridge-side time. Item error counters
include all requested items on a disconnected call. Bad quality is counted
separately from failed HRESULTs. These are observed cumulative counts, not
synthetic DA metrics; use Rust's end-to-end timings separately. `{"op":"stats"}`
returns them without a DA data operation.

`{"op":"close"}` returns final statistics, releases the interfaces/removes the
group, then exits. EOF also releases all COM objects. After all clients exit,
remove only this simulator's per-user registration:

```fish
wine regsvr32 /u /s "$dll"
wineserver -w
```

The isolated prefix can then be discarded by its owner. Neither startup nor
shutdown touches existing production endpoints, certificates or secrets.

## Primary sources and licensing

- OPC Foundation's [opcda.idl at the pinned source revision](https://github.com/OPCF-Members/UA-.NET-Legacy/blob/422436e0d0cc523c1a7b935dd12b987a3036b700/ComIOP/Wrapper/Include/opcda.idl)
  defines the interface UUIDs, vtable order and structures used in `opc_da.hpp`.
  This IDL carries the OPC Foundation MIT License 1.00, preserved in
  `LICENSE.OPC`. No commercial SDK implementation or proprietary error header
  is copied into this repository.
- OPC Foundation's [COM DA read mapping](https://reference.opcfoundation.org/specs/OPC-10000-8/a-3-3)
  describes the DA 2.05a AddGroup → AddItems → IOPCSyncIO::Read path and VQT/error
  mapping. This implementation exercises that synchronous path only.
- Microsoft's [IUnknown::QueryInterface contract](https://learn.microsoft.com/en-us/windows/win32/api/unknwn/nf-unknwn-iunknown-queryinterface%28refiid_void%29)
  defines stable identity, reference ownership and unsupported-interface errors.
- [RFC 8259](https://www.rfc-editor.org/rfc/rfc8259) defines the JSON wire syntax.

Original simulator, bridge, parser and tests follow the repository's Apache-2.0
license. Native test output and complete campaign artifacts must be captured by
the integration runner before making any interoperability/performance claim.
