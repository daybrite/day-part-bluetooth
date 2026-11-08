package dev.daybrite.blefixture;

import android.Manifest;
import android.app.Activity;
import android.bluetooth.*;
import android.bluetooth.le.*;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelUuid;
import android.view.WindowManager;
import android.widget.TextView;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

/** Standalone, disposable GATT fixture: no accounts, real sensor data, or background service. */
public final class MainActivity extends Activity {
    private static final UUID SERVICE = UUID.fromString("e55733e0-da7b-4c45-ac2d-5d559a122f01");
    private static final UUID VALUE = UUID.fromString("e55733e1-da7b-4c45-ac2d-5d559a122f01");
    private static final UUID CCCD = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");
    private final Handler main = new Handler(Looper.getMainLooper());
    private final Set<BluetoothDevice> subscribers = ConcurrentHashMap.newKeySet();
    private volatile byte[] value = "day-fixture-ready".getBytes(StandardCharsets.UTF_8);
    private volatile BluetoothGattServer server;
    private BluetoothGattCharacteristic characteristic;
    private BluetoothLeAdvertiser advertiser;
    private TextView status;
    private final AdvertiseCallback advertising = new AdvertiseCallback() {
        @Override public void onStartSuccess(AdvertiseSettings settings) { show(R.string.advertising); }
        @Override public void onStartFailure(int error) { show(R.string.unavailable); }
    };
    @Override public void onCreate(Bundle saved) {
        super.onCreate(saved);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        status = new TextView(this);
        status.setTextSize(22);
        status.setPadding(36, 64, 36, 36);
        status.setText(R.string.starting);
        setContentView(status);
        if (checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) != PackageManager.PERMISSION_GRANTED
                || checkSelfPermission(Manifest.permission.BLUETOOTH_ADVERTISE) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.BLUETOOTH_CONNECT, Manifest.permission.BLUETOOTH_ADVERTISE}, 1);
            show(R.string.unavailable);
            return;
        }
        startFixture();
    }
    private void show(int resource) { main.post(() -> status.setText(resource)); }
    private void startFixture() {
        BluetoothManager manager = getSystemService(BluetoothManager.class);
        BluetoothAdapter adapter = manager == null ? null : manager.getAdapter();
        if (adapter == null || !adapter.isEnabled()) { show(R.string.unavailable); return; }
        advertiser = adapter.getBluetoothLeAdvertiser();
        if (advertiser == null) { show(R.string.unavailable); return; }
        characteristic = new BluetoothGattCharacteristic(VALUE,
                BluetoothGattCharacteristic.PROPERTY_READ | BluetoothGattCharacteristic.PROPERTY_WRITE | BluetoothGattCharacteristic.PROPERTY_NOTIFY,
                BluetoothGattCharacteristic.PERMISSION_READ | BluetoothGattCharacteristic.PERMISSION_WRITE);
        characteristic.addDescriptor(new BluetoothGattDescriptor(CCCD, BluetoothGattDescriptor.PERMISSION_READ | BluetoothGattDescriptor.PERMISSION_WRITE));
        BluetoothGattService service = new BluetoothGattService(SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY);
        service.addCharacteristic(characteristic);
        server = manager.openGattServer(this, callbacks);
        if (server == null || !server.addService(service)) { stopFixture(); show(R.string.unavailable); return; }
        main.postDelayed(() -> { stopFixture(); show(R.string.finished); }, 180_000);
    }
    private final BluetoothGattServerCallback callbacks = new BluetoothGattServerCallback() {
        @Override public void onServiceAdded(int result, BluetoothGattService service) {
            if (result != BluetoothGatt.GATT_SUCCESS) { show(R.string.unavailable); return; }
            main.post(() -> {
                if (server == null) return;
                advertiser.startAdvertising(new AdvertiseSettings.Builder().setConnectable(true)
                        .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY).setTimeout(180_000).build(),
                        new AdvertiseData.Builder().addServiceUuid(new ParcelUuid(SERVICE)).build(), advertising);
            });
        }
        @Override public void onConnectionStateChange(BluetoothDevice device, int result, int state) {
            if (state == BluetoothProfile.STATE_DISCONNECTED) subscribers.remove(device);
        }
        @Override public void onCharacteristicReadRequest(BluetoothDevice device, int request, int offset, BluetoothGattCharacteristic target) {
            byte[] snapshot = value;
            BluetoothGattServer active = server;
            if (active != null) active.sendResponse(device, request,
                    VALUE.equals(target.getUuid()) && offset <= snapshot.length ? BluetoothGatt.GATT_SUCCESS : BluetoothGatt.GATT_INVALID_OFFSET,
                    offset, offset <= snapshot.length ? Arrays.copyOfRange(snapshot, offset, snapshot.length) : null);
        }
        @Override public void onCharacteristicWriteRequest(BluetoothDevice device, int request, BluetoothGattCharacteristic target, boolean prepared, boolean response, int offset, byte[] bytes) {
            boolean valid = VALUE.equals(target.getUuid()) && !prepared && offset == 0 && bytes != null && bytes.length <= 64;
            BluetoothGattServer active = server;
            if (active == null) return;
            if (valid) value = bytes.clone();
            if (response) active.sendResponse(device, request, valid ? BluetoothGatt.GATT_SUCCESS : BluetoothGatt.GATT_INVALID_ATTRIBUTE_LENGTH, offset, null);
            if (valid) {
                characteristic.setValue(value);
                for (BluetoothDevice subscriber : subscribers) active.notifyCharacteristicChanged(subscriber, characteristic, false);
            }
        }
        @Override public void onDescriptorReadRequest(BluetoothDevice device, int request, int offset, BluetoothGattDescriptor descriptor) {
            BluetoothGattServer active = server;
            if (active != null) active.sendResponse(device, request, offset == 0 ? BluetoothGatt.GATT_SUCCESS : BluetoothGatt.GATT_INVALID_OFFSET,
                    offset, subscribers.contains(device) ? BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE : BluetoothGattDescriptor.DISABLE_NOTIFICATION_VALUE);
        }
        @Override public void onDescriptorWriteRequest(BluetoothDevice device, int request, BluetoothGattDescriptor descriptor, boolean prepared, boolean response, int offset, byte[] bytes) {
            boolean enable = Arrays.equals(bytes, BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE);
            boolean valid = CCCD.equals(descriptor.getUuid()) && !prepared && offset == 0
                    && (enable || Arrays.equals(bytes, BluetoothGattDescriptor.DISABLE_NOTIFICATION_VALUE));
            if (valid) { if (enable) subscribers.add(device); else subscribers.remove(device); }
            BluetoothGattServer active = server;
            if (response && active != null) active.sendResponse(device, request, valid ? BluetoothGatt.GATT_SUCCESS : BluetoothGatt.GATT_REQUEST_NOT_SUPPORTED, offset, null);
        }
    };
    private void stopFixture() {
        if (advertiser != null) advertiser.stopAdvertising(advertising);
        BluetoothGattServer active = server;
        server = null;
        if (active != null) active.close();
        subscribers.clear();
    }
    @Override public void onDestroy() { main.removeCallbacksAndMessages(null); stopFixture(); super.onDestroy(); }
}
