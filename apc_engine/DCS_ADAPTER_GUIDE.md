# DCS Adapter Guide for AuTHRity MPC System

This document describes the generic MPC mode protocol and required OPC UA nodes used by `apc_engine`.

---

## Generic MPC Protocol

The MPC system uses a **vendor-neutral mode protocol**. DCS-specific adapters must translate native modes to the values below.

### MPC Mode Values

| Value | Name | Description |
|-------|------|-------------|
| **0** | Operator Control | Manual, Initialization Manual, Out of Service |
| **1** | Local Auto | PID controls to local setpoint |
| **2** | Cascade | PID accepts setpoint from upstream controller |
| **3** | Remote Cascade | PID accepts setpoint from MPC ← **MPC writes here** |

### Required OPC UA Nodes (per MV)

For each Manipulated Variable, provide these nodes (names are examples):

```yaml
MVName:PV           # Process measurement (Double)
MVName:SP           # Setpoint input for MPC (Double)
MVName:OP           # Controller output to field (Double)
MVName:Mode         # Current mode 0-3 (Int32)
MVName:ModeTarget   # Requested mode 0-3 (Int32)
MVName:FuturePlan   # Control horizon visualization (Double[])
MVName:HighLimit    # MV high limit
MVName:LowLimit     # MV low limit
MVName:HighHighLimit# MV high-high limit (safety)
MVName:LowLowLimit  # MV low-low limit (safety)
```

---

## Bumpless Transfer Handshake

The MPC implements a standard bumpless transfer sequence:

1. **Operator Action**: Sets `ModeTarget = 3` (Remote Cascade)
2. **MPC Response**: Writes current `OP` value to `SP` (no bump)
3. **DCS Response**: Transitions `Mode → 3`
4. **MPC Control**: Begins sending calculated moves

**Timeline:**
```
Scan 1: Mode=1, Target=3  → MPC writes bumpless SP
Scan 2: Mode=3, Target=3  → MPC writes first controller move
Scan 3+: Normal operation
```

---

## OPC DA / ABB integration

The production engine remains an OPC UA **client**. An ABB installation exposing
only OPC DA needs a separately validated DA-to-UA gateway or production Windows
adapter. This repository now includes an isolated native synchronous COM DA
server/client in [`opc_da_simulator/`](../opc_da_simulator/README.md) and a
`mpc_campaign` transport adapter for the synthetic closed-loop evaluation.
That local COM profile is not a remote DCOM service, ABB emulator, or permission
to connect the production engine directly to ABB DA.

The campaign keeps command SP and actual actuator feedback separate. Its learned
SP-to-CV model uses command readback as the controller input coordinate; actual
actuator dynamics remain in the independent simulated plant. The original
production OP/SP mapping below still requires commissioning.

Confirm the actual ABB controller/system version, installed server options,
licenses, readable/writable tags, quality and timestamp mapping, update rate,
and native mode values. The generic 0–3 modes above are not ABB native constants.
Do not infer writable UA support from the ABB name or a version number alone.

**Engineering-unit gate:** the current engine reads `node_ids.op` as current MV
and writes `current MV + delta` to `node_ids.sp`; the bumpless path likewise copies
OP to SP. A real PID OP (valve %) and SP (flow/temperature units) are not generally
interchangeable. Establish a consistent manipulated-coordinate mapping before
any plant writes. An ideal simulator with OP=SP does not validate this mapping.

See [historical MPC validation design](../DIGITAL_TWIN_VALIDATION.md) for the
separate mathematical, OPC, shadow-mode and authorized commissioning gates.
Primary references: [ABB AC 800M OPC Server](https://library.e.abb.com/public/01118fad328ee7b3c1257dc00039f23e/3BSE035983-600_-_en_AC_800M_6.0_OPC_Server.pdf)
and [Unified Automation UaGateway](https://documentation.unified-automation.com/uagateway/1.6.1/html/index.html).
