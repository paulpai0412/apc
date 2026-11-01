// Register the existing simulator in an x64 process, matching its DLL ABI.
// The container entrypoint owns a dedicated Wine prefix; no host registration.
#include <windows.h>
#include <iostream>
int main(int argc, char** argv) {
    if (argc != 2) return 2;
    HMODULE dll = LoadLibraryA(argv[1]);
    if (!dll) { std::cerr << "Simulator LoadLibrary failed: " << GetLastError() << "\n"; return 1; }
    auto entry = reinterpret_cast<HRESULT (WINAPI*)()>(GetProcAddress(dll, "DllRegisterServer"));
    const HRESULT result = entry ? entry() : E_NOINTERFACE;
    FreeLibrary(dll);
    if (FAILED(result)) { std::cerr << "Simulator registration failed: " << result << "\n"; return 1; }
    return 0;
}
