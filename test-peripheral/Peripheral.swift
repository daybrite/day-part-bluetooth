// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// Synthetic GATT fixture. Run on a Mac; connect from a separate physical BLE device.
import Foundation
import CoreBluetooth
import Darwin

final class Fixture: NSObject, CBPeripheralManagerDelegate {
    let serviceID = CBUUID(string: "E55733E0-DA7B-4C45-AC2D-5D559A122F01")
    let valueID = CBUUID(string: "E55733E1-DA7B-4C45-AC2D-5D559A122F01")
    var manager: CBPeripheralManager!
    var characteristic: CBMutableCharacteristic!
    var value = Data("day-fixture-ready".utf8)
    var pending: Data?
    override init() {
        super.init()
        manager = CBPeripheralManager(delegate: self, queue: .main)
    }
    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        guard peripheral.state == .poweredOn else {
            print("Bluetooth state: \(peripheral.state.rawValue)")
            return
        }
        characteristic = CBMutableCharacteristic(type: valueID, properties: [.read, .write, .notify], value: nil, permissions: [.readable, .writeable])
        let service = CBMutableService(type: serviceID, primary: true)
        service.characteristics = [characteristic]
        peripheral.add(service)
    }
    func peripheralManager(_ peripheral: CBPeripheralManager, didAdd service: CBService, error: Error?) {
        if let error { print("Service failed: \(error)"); exit(1) }
        peripheral.startAdvertising([CBAdvertisementDataLocalNameKey: "DayBLEFixture", CBAdvertisementDataServiceUUIDsKey: [serviceID]])
    }
    func peripheralManagerDidStartAdvertising(_ peripheral: CBPeripheralManager, error: Error?) {
        if let error { print("Advertising failed: \(error)"); exit(1) }
        print("Advertising DayBLEFixture; stops after ten minutes")
    }
    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveRead request: CBATTRequest) {
        guard request.characteristic.uuid == valueID else { peripheral.respond(to: request, withResult: .attributeNotFound); return }
        guard request.offset <= value.count else { peripheral.respond(to: request, withResult: .invalidOffset); return }
        request.value = value.subdata(in: request.offset..<value.count)
        peripheral.respond(to: request, withResult: .success)
        print("Read \(request.value?.count ?? 0) bytes")
    }
    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveWrite requests: [CBATTRequest]) {
        guard let first = requests.first else { return }
        guard requests.allSatisfy({ $0.characteristic.uuid == valueID && $0.offset == 0 && ($0.value?.count ?? 0) <= 64 }) else {
            peripheral.respond(to: first, withResult: .invalidAttributeValueLength)
            return
        }
        for request in requests { value = request.value ?? Data() }
        peripheral.respond(to: first, withResult: .success)
        if !peripheral.updateValue(value, for: characteristic, onSubscribedCentrals: nil) { pending = value }
        print("Write \(value.count) bytes")
    }
    func peripheralManager(_ peripheral: CBPeripheralManager, central: CBCentral, didSubscribeTo characteristic: CBCharacteristic) {
        print("Notification subscription enabled")
    }
    func peripheralManagerIsReady(toUpdateSubscribers peripheral: CBPeripheralManager) {
        if let value = pending, peripheral.updateValue(value, for: characteristic, onSubscribedCentrals: nil) { pending = nil }
    }
}
setbuf(stdout, nil)
let fixture = Fixture()
DispatchQueue.main.asyncAfter(deadline: .now() + 600) {
    fixture.manager.stopAdvertising()
    fixture.manager.removeAllServices()
    exit(0)
}
RunLoop.main.run()
