#!/bin/sh
set -eu
umask 077
mkdir -p /data/config /data/research /data/reports /data/pki
rm -f /tmp/apc-com-ready
# Re-register this image's DLL in this service's dedicated, persisted prefix.
# A failed real COM acceptance must prevent the HMI from claiming readiness.
wineboot --init
simulator=$(winepath -w /app/com/authrity_opc_da.dll)
wine /app/com/apc_register.exe "$simulator"
wine reg copy 'HKCU\Software\Classes\CLSID\{BCE86C29-5DCB-43D8-A6A0-4699B503A0B8}' 'HKLM\Software\Classes\CLSID\{BCE86C29-5DCB-43D8-A6A0-4699B503A0B8}' /s /f
wine reg copy 'HKCU\Software\Classes\AuTHRity.LocalDaSimulator.1' 'HKLM\Software\Classes\AuTHRity.LocalDaSimulator.1' /s /f
wine /app/com/opc_da_tests.exe
printf 'real-com-tests-passed\n' > /tmp/apc-com-ready
exec /app/bin/authrity-hmi
