CXX = x86_64-w64-mingw32-g++
CPPFLAGS = -D_WIN32_WINNT=0x0601 -DUNICODE -D_UNICODE
CXXFLAGS = -std=c++17 -O2 -Wall -Wextra
LDFLAGS = -static -static-libgcc -static-libstdc++
LDLIBS = -lole32 -loleaut32 -ladvapi32 -luuid
OUT = build

.PHONY: all clean
all: $(OUT)/authrity_opc_da.dll $(OUT)/opc_da_bridge.exe $(OUT)/opc_da_tests.exe

$(OUT):
	mkdir -p $(OUT)

$(OUT)/authrity_opc_da.dll: server.cpp opc_da.hpp server.def | $(OUT)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -shared server.cpp server.def -o $@ $(LDFLAGS) $(LDLIBS)

$(OUT)/opc_da_bridge.exe: bridge.cpp opc_da.hpp json.hpp | $(OUT)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) bridge.cpp -o $@ $(LDFLAGS) $(LDLIBS)

$(OUT)/opc_da_tests.exe: tests.cpp opc_da.hpp json.hpp | $(OUT)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) tests.cpp -o $@ $(LDFLAGS) $(LDLIBS)

clean:
	rm -f $(OUT)/authrity_opc_da.dll $(OUT)/opc_da_bridge.exe $(OUT)/opc_da_tests.exe
