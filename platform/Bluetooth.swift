import Foundation
import CoreBluetooth

private final class DayBluetoothCentral: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    static let shared = DayBluetoothCentral()
    private var manager: CBCentralManager!
    private var observers = Set<UInt64>()
    private var devices: [String: CBPeripheral] = [:]
    private struct Request {
        let token: UInt64
        let body: [String: Any]
        let op: String
        let device: String
        var characteristic: String { body["characteristic"] as? String ?? "" }
        var service: String { body["service"] as? String ?? "" }
    }
    private var pending: [String: Request] = [:]
    private var queues: [String: [Request]] = [:]
    private var closing = Set<String>()
    private var remainingServices: [String: Set<ObjectIdentifier>] = [:]
    private var bufferedWrites: [String: (Data, CBCharacteristic)] = [:]

    func prepare() {
        if manager == nil { manager = CBCentralManager(delegate: self, queue: .main) }
    }
    func observe(_ token: UInt64) {
        observers.insert(token)
        prepare()
        emit(["event": "state", "state": stateName(manager.state)])
    }
    func unobserve(_ token: UInt64) { observers.remove(token) }
    private func encode(_ value: Any) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed]),
              let text = String(data: data, encoding: .utf8) else { return "null" }
        return text
    }
    private func emit(_ value: Any) {
        let text = encode(value)
        for observer in observers { watch_native_emit(observer, text) }
    }
    private func reply(_ token: UInt64, _ value: Any = NSNull()) {
        completed(token)
        request_native_complete(token, encode(["result": value]))
    }
    private func fail(_ token: UInt64, _ kind: String, _ message: String) {
        completed(token)
        request_native_complete(token, encode(["error": ["kind": kind, "message": message]]))
    }
    private func completed(_ token: UInt64) {
        guard let current = pending.values.first(where: { $0.token == token }) else { return }
        pending.removeValue(forKey: current.device)
        remainingServices.removeValue(forKey: current.device)
        bufferedWrites.removeValue(forKey: current.device)
        DispatchQueue.main.async { self.next(current.device) }
    }
    private func next(_ id: String) {
        guard pending[id] == nil, !closing.contains(id), var queue = queues[id], !queue.isEmpty else { return }
        let request = queue.removeFirst()
        queues[id] = queue.isEmpty ? nil : queue
        pending[id] = request
        execute(request)
    }
    private func drain(_ id: String, _ kind: String, _ message: String) {
        let current = pending.removeValue(forKey: id)
        let queued = queues.removeValue(forKey: id) ?? []
        remainingServices.removeValue(forKey: id)
        bufferedWrites.removeValue(forKey: id)
        if let current { fail(current.token, kind, message) }
        for request in queued { fail(request.token, kind, message) }
    }
    func cancel(_ token: UInt64) {
        if let current = pending.values.first(where: { $0.token == token }) {
            let id = current.device
            drain(id, "disconnected", "An active GATT operation was cancelled; reconnect before continuing")
            if let device = devices[id], device.state != .disconnected {
                closing.insert(id)
                manager.cancelPeripheralConnection(device)
            }
        } else {
            for id in Array(queues.keys) {
                queues[id]?.removeAll { $0.token == token }
                if queues[id]?.isEmpty == true { queues.removeValue(forKey: id) }
            }
        }
    }
    private func finish(_ id: String, _ value: Any = NSNull(), error: Error? = nil) {
        guard let current = pending[id] else { return }
        if let error { fail(current.token, "platform", error.localizedDescription) }
        else { reply(current.token, value) }
    }
    private func stateName(_ state: CBManagerState) -> String {
        switch state {
        case .unknown: return "unknown"
        case .resetting: return "resetting"
        case .unsupported: return "unsupported"
        case .unauthorized: return "unauthorized"
        case .poweredOff: return "powered_off"
        case .poweredOn: return "powered_on"
        @unknown default: return "unknown"
        }
    }
    func command(_ text: String, _ token: UInt64) {
        prepare()
        guard let data = text.data(using: .utf8),
              let request = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let op = request["op"] as? String else {
            fail(token, "invalid_argument", "Malformed Bluetooth request"); return
        }
        let id = request["device"] as? String ?? ""
        let value = Request(token: token, body: request, op: op, device: id)
        if ["connect", "services", "read", "write", "notify"].contains(op) {
            guard !closing.contains(id) else { fail(token, "disconnected", "Peripheral is disconnecting"); return }
            guard (queues[id]?.count ?? 0) < 128 else { fail(token, "busy", "Peripheral request queue is full"); return }
            queues[id, default: []].append(value)
            next(id)
        } else { execute(value) }
    }
    private func execute(_ current: Request) {
        let request = current.body, op = current.op, id = current.device, token = current.token
        if op == "stop_scan" { manager.stopScan(); reply(token); return }
        if op == "disconnect" {
            drain(id, "disconnected", "Peripheral disconnected by app")
            if let device = devices[id], device.state != .disconnected {
                closing.insert(id)
                manager.cancelPeripheralConnection(device)
            }
            reply(token); return
        }
        guard manager.state == .poweredOn else {
            let kind = manager.state == .unauthorized ? "permission_denied" :
                manager.state == .poweredOff ? "powered_off" :
                manager.state == .unsupported ? "unsupported" : "busy"
            fail(token, kind, "Bluetooth adapter is \(stateName(manager.state))"); return
        }
        if op == "scan" {
            let services = (request["services"] as? [String] ?? []).map { CBUUID(string: $0) }
            manager.scanForPeripherals(withServices: services.isEmpty ? nil : services,
                options: [CBCentralManagerScanOptionAllowDuplicatesKey: request["duplicates"] as? Bool ?? false])
            reply(token); return
        }
        if devices[id] == nil, let uuid = UUID(uuidString: id),
           let restored = manager.retrievePeripherals(withIdentifiers: [uuid]).first { devices[id] = restored }
        guard let device = devices[id] else { fail(token, "not_found", "Unknown peripheral"); return }
        device.delegate = self
        if op != "connect" && device.state != .connected {
            fail(token, "disconnected", "Peripheral is disconnected"); return
        }
        let characteristicID = request["characteristic"] as? String ?? ""
        DispatchQueue.main.asyncAfter(deadline: .now() + 20) { [weak self] in
            guard let self, self.pending[id]?.token == token else { return }
            self.closing.insert(id)
            self.drain(id, "timeout", "Bluetooth operation timed out; connection closed")
            self.manager.cancelPeripheralConnection(device)
        }
        if op == "connect" {
            if device.state == .connected { finish(id) }
            else { manager.connect(device) }
            return
        }
        if op == "services" { device.discoverServices(nil); return }
        guard let serviceID = request["service"] as? String,
              let service = device.services?.first(where: { $0.uuid == CBUUID(string: serviceID) }),
              let characteristic = service.characteristics?.first(where: { $0.uuid == CBUUID(string: characteristicID) }) else {
            fail(token, "not_found", "Discover the service and characteristic first"); return
        }
        switch op {
        case "read":
            guard characteristic.properties.contains(.read) else { fail(token, "unsupported", "Characteristic is not readable"); return }
            device.readValue(for: characteristic)
        case "write":
            let response = request["with_response"] as? Bool ?? true
            let type: CBCharacteristicWriteType = response ? .withResponse : .withoutResponse
            let bytes = request["value"] as? [UInt8] ?? []
            let property: CBCharacteristicProperties = response ? .write : .writeWithoutResponse
            guard characteristic.properties.contains(property), bytes.count <= device.maximumWriteValueLength(for: type) else {
                fail(token, "invalid_argument", "Write type or payload size is unsupported"); return
            }
            if !response && !device.canSendWriteWithoutResponse {
                bufferedWrites[id] = (Data(bytes), characteristic)
                return
            }
            device.writeValue(Data(bytes), for: characteristic, type: type)
            if !response { finish(id) }
        case "notify":
            guard characteristic.properties.contains(.notify) || characteristic.properties.contains(.indicate) else {
                fail(token, "unsupported", "Characteristic has no notifications"); return
            }
            device.setNotifyValue(request["enabled"] as? Bool ?? false, for: characteristic)
        default:
            fail(token, "invalid_argument", "Unknown Bluetooth operation")
        }
    }
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        emit(["event": "state", "state": stateName(central.state)])
        if central.state != .poweredOn {
            for id in Set(pending.keys).union(queues.keys) { drain(id, "disconnected", "Bluetooth adapter became unavailable") }
            for (id, device) in devices where device.state != .disconnected {
                emit(["event": "disconnected", "device": id])
            }
            closing.removeAll()
            devices.removeAll()
        }
    }
    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral, advertisementData: [String: Any], rssi RSSI: NSNumber) {
        let id = peripheral.identifier.uuidString
        devices[id] = peripheral
        let services = (advertisementData[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID] ?? []).map(\.uuidString)
        let bytes = Array(advertisementData[CBAdvertisementDataManufacturerDataKey] as? Data ?? Data())
        emit(["event": "advertisement", "advertisement": ["device": id,
            "name": advertisementData[CBAdvertisementDataLocalNameKey] as? String ?? peripheral.name as Any? ?? NSNull(),
            "rssi": RSSI, "services": services, "manufacturer_data": bytes,
            "connectable": advertisementData[CBAdvertisementDataIsConnectable] as? Bool as Any? ?? NSNull()]])
    }
    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        let id = peripheral.identifier.uuidString
        if closing.contains(id) { central.cancelPeripheralConnection(peripheral); return }
        emit(["event": "connected", "device": id])
        if pending[id]?.op == "connect" { finish(id) }
    }
    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        let id = peripheral.identifier.uuidString
        closing.remove(id)
        drain(id, "disconnected", error?.localizedDescription ?? "Connection failed")
    }
    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        let id = peripheral.identifier.uuidString
        closing.remove(id)
        drain(id, "disconnected", error?.localizedDescription ?? "Disconnected")
        emit(["event": "disconnected", "device": id])
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        let id = peripheral.identifier.uuidString
        guard pending[id]?.op == "services", !closing.contains(id) else { return }
        if let error {
            // Other discovery callbacks may still be queued; close before another request can reuse them.
            closing.insert(id)
            drain(id, "platform", error.localizedDescription)
            manager.cancelPeripheralConnection(peripheral)
            return
        }
        let services = peripheral.services ?? []
        remainingServices[id] = Set(services.map { ObjectIdentifier($0) })
        if services.isEmpty { finish(id, [Any]()); return }
        for service in services { peripheral.discoverCharacteristics(nil, for: service) }
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        let id = peripheral.identifier.uuidString
        guard pending[id]?.op == "services", !closing.contains(id) else { return }
        if let error {
            // Other discovery callbacks may still be queued; close before another request can reuse them.
            closing.insert(id)
            drain(id, "platform", error.localizedDescription)
            manager.cancelPeripheralConnection(peripheral)
            return
        }
        guard remainingServices[id]?.remove(ObjectIdentifier(service)) != nil else { return }
        if remainingServices[id]?.isEmpty == true {
            let result: [[String: Any]] = (peripheral.services ?? []).map { service in
                ["uuid": service.uuid.uuidString, "characteristics": (service.characteristics ?? []).map { c in
                    ["uuid": c.uuid.uuidString, "read": c.properties.contains(.read), "write": c.properties.contains(.write),
                     "write_without_response": c.properties.contains(.writeWithoutResponse), "notify": c.properties.contains(.notify),
                     "indicate": c.properties.contains(.indicate)] as [String: Any]
                }]
            }
            finish(id, result)
        }
    }
    func peripheralIsReady(toSendWriteWithoutResponse peripheral: CBPeripheral) {
        let id = peripheral.identifier.uuidString
        guard !closing.contains(id), pending[id]?.op == "write", peripheral.canSendWriteWithoutResponse,
              let (data, characteristic) = bufferedWrites.removeValue(forKey: id) else { return }
        peripheral.writeValue(data, for: characteristic, type: .withoutResponse)
        finish(id)
    }
    func peripheral(_ peripheral: CBPeripheral, didModifyServices invalidatedServices: [CBService]) {
        let id = peripheral.identifier.uuidString
        // Old characteristic handles must not complete a request against the new service table.
        drain(id, "disconnected", "Peripheral services changed; reconnect and discover services again")
        closing.insert(id)
        manager.cancelPeripheralConnection(peripheral)
    }
    private func matches(_ op: String, _ peripheral: CBPeripheral, _ characteristic: CBCharacteristic) -> Bool {
        let id = peripheral.identifier.uuidString
        guard !closing.contains(id), let current = pending[id], let service = characteristic.service else { return false }
        return current.op == op && CBUUID(string: current.service) == service.uuid &&
            CBUUID(string: current.characteristic) == characteristic.uuid
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        let value = Array(characteristic.value ?? Data())
        if matches("read", peripheral, characteristic) { finish(peripheral.identifier.uuidString, value, error: error) }
        if error == nil && peripheral.state == .connected && !closing.contains(peripheral.identifier.uuidString) {
            emit(["event": "value", "device": peripheral.identifier.uuidString,
                  "service": characteristic.service?.uuid.uuidString ?? "", "characteristic": characteristic.uuid.uuidString, "value": value])
        }
    }
    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if matches("write", peripheral, characteristic) { finish(peripheral.identifier.uuidString, error: error) }
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        if matches("notify", peripheral, characteristic) { finish(peripheral.identifier.uuidString, error: error) }
    }
}

func request_native(request: String, done: UInt64) throws {
    DispatchQueue.main.async { DayBluetoothCentral.shared.command(request, done) }
}
func watch_native(emit: UInt64) throws {
    DispatchQueue.main.async { DayBluetoothCentral.shared.observe(emit) }
}
func unwatch_native(token: Int64) throws {
    DispatchQueue.main.async { DayBluetoothCentral.shared.unobserve(UInt64(bitPattern: token)) }
}

func cancel_native(token: Int64) throws {
    DispatchQueue.main.async { DayBluetoothCentral.shared.cancel(UInt64(bitPattern: token)) }
}
