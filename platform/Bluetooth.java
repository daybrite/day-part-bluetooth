import android.bluetooth.*;
import android.bluetooth.le.*;
import android.content.*;
import android.os.*;
import dev.daybrite.day.bridge.DayRuntime;
import org.json.*;
import java.util.*;

private static final Handler main = new Handler(Looper.getMainLooper());
private static final Set<Long> observers = new HashSet<>();
private static final Map<String, BluetoothGatt> connections = new HashMap<>();
private static final Map<String, BluetoothDevice> devices = new HashMap<>();
private static final Set<String> seen = new HashSet<>();
private static final class Request {
    final JSONObject body;
    final long token;
    final String device, operation, service, characteristic;
    Request(JSONObject body, long token) {
        this.body = body; this.token = token;
        device = body.optString("device"); operation = body.optString("op");
        service = body.optString("service"); characteristic = body.optString("characteristic");
    }
}
private static final Map<String, Request> active = new HashMap<>();
private static final Map<String, ArrayDeque<Request>> queues = new HashMap<>();
private static final Set<String> connected = new HashSet<>();
private static boolean duplicates;
private static boolean scanning;
private static boolean receiverInstalled;
private static BluetoothAdapter adapter;

private static JSONObject object(Object... pairs) {
    JSONObject out = new JSONObject();
    try { for (int i = 0; i < pairs.length; i += 2) out.put((String)pairs[i], pairs[i+1]); }
    catch (JSONException e) { throw new IllegalArgumentException(e); }
    return out;
}
private static JSONArray bytes(byte[] bytes) {
    JSONArray out = new JSONArray();
    if (bytes != null) for (byte value : bytes) out.put(value & 255);
    return out;
}
private static UUID uuid(String value) {
    if (value.length() == 4) value = "0000" + value;
    if (value.length() == 8) value += "-0000-1000-8000-00805f9b34fb";
    return UUID.fromString(value);
}
private static void emit(JSONObject event) {
    for (long observer : new ArrayList<>(observers)) watch_native_emit(observer, event.toString());
}
private static void reply(long token, Object value) {
    completed(token);
    request_native_complete(token, object("result", value == null ? JSONObject.NULL : value).toString());
}
private static void fail(long token, String kind, String message) {
    completed(token);
    request_native_complete(token, object("error", object("kind", kind, "message", message)).toString());
}
private static void completed(long token) {
    Request request = null;
    for (Request value : active.values()) if (value.token == token) { request = value; break; }
    if (request == null) return;
    active.remove(request.device);
    String id = request.device;
    main.post(() -> next(id));
}
private static void next(String id) {
    if (active.containsKey(id)) return;
    ArrayDeque<Request> queue = queues.get(id);
    if (queue == null || queue.isEmpty()) { queues.remove(id); return; }
    Request request = queue.removeFirst();
    active.put(id, request);
    execute(request);
}
private static void drain(String id, String kind, String message) {
    ArrayDeque<Request> queue = queues.remove(id);
    Request request = active.remove(id);
    if (request != null) fail(request.token, kind, message);
    if (queue != null) for (Request queued : queue) fail(queued.token, kind, message);
}
private static void finish(String id, Object value, int status) {
    Request request = active.get(id);
    if (request == null) return;
    if (status == BluetoothGatt.GATT_SUCCESS) reply(request.token, value);
    else fail(request.token, "platform", "GATT status " + status);
}
private static boolean current(BluetoothGatt gatt) {
    return connections.get(gatt.getDevice().getAddress()) == gatt;
}
private static boolean scanNeedsLocation() {
    if (Build.VERSION.SDK_INT < 31) return true;
    try {
        android.content.pm.PackageInfo info = DayRuntime.ctx.getPackageManager().getPackageInfo(
            DayRuntime.ctx.getPackageName(), android.content.pm.PackageManager.GET_PERMISSIONS);
        if (info.requestedPermissions != null) for (int i = 0; i < info.requestedPermissions.length; i++) {
            if ("android.permission.BLUETOOTH_SCAN".equals(info.requestedPermissions[i])) {
                return (info.requestedPermissionsFlags[i] & android.content.pm.PackageInfo.REQUESTED_PERMISSION_NEVER_FOR_LOCATION) == 0;
            }
        }
    } catch (android.content.pm.PackageManager.NameNotFoundException ignored) {}
    return true;
}
private static boolean permitted() {
    Context context = DayRuntime.ctx;
    boolean nearby = Build.VERSION.SDK_INT < 31 ||
        (context.checkSelfPermission("android.permission.BLUETOOTH_SCAN") == 0 && context.checkSelfPermission("android.permission.BLUETOOTH_CONNECT") == 0);
    return nearby && (!scanNeedsLocation() || context.checkSelfPermission("android.permission.ACCESS_FINE_LOCATION") == 0);
}
private static String state() {
    if (!permitted()) return "unauthorized";
    if (adapter == null) return "unsupported";
    return adapter.isEnabled() ? "powered_on" : "powered_off";
}
private static void prepare() {
    Context context = DayRuntime.ctx;
    if (context == null) throw new IllegalStateException("Day Android runtime is not initialized");
    BluetoothManager manager = (BluetoothManager)context.getSystemService(Context.BLUETOOTH_SERVICE);
    adapter = manager == null ? null : manager.getAdapter();
    if (!receiverInstalled) {
        BroadcastReceiver receiver = new BroadcastReceiver() {
            public void onReceive(Context ctx, Intent intent) {
                main.post(() -> {
                    emit(object("event", "state", "state", state()));
                    if (!"powered_on".equals(state())) {
                        scanning = false;
                        for (BluetoothGatt gatt : new ArrayList<>(connections.values())) gatt.close();
                        for (String id : connections.keySet()) emit(object("event", "disconnected", "device", id));
                        connections.clear(); connected.clear();
                        Set<String> affected = new HashSet<>(active.keySet());
                        affected.addAll(queues.keySet());
                        for (String id : affected) drain(id, "disconnected", "Bluetooth became unavailable");
                    }
                });
            }
        };
        if (Build.VERSION.SDK_INT >= 33) context.getApplicationContext().registerReceiver(receiver, new IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED), Context.RECEIVER_EXPORTED);
        else context.getApplicationContext().registerReceiver(receiver, new IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED));
        receiverInstalled = true;
    }
}
private static final ScanCallback scanner = new ScanCallback() {
    public void onScanResult(int type, ScanResult result) { main.post(() -> discovered(result)); }
    public void onBatchScanResults(List<ScanResult> results) { main.post(() -> { for (ScanResult result : results) discovered(result); }); }
    public void onScanFailed(int code) {
        main.post(() -> { scanning = false; emit(object("event", "error", "error", object("kind", "platform", "message", "Scan failed: " + code))); });
    }
};
private static void discovered(ScanResult result) {
    if (!scanning) return;
    String id = result.getDevice().getAddress();
    devices.put(id, result.getDevice());
    if (!duplicates && !seen.add(id)) return;
    ScanRecord record = result.getScanRecord();
    JSONArray services = new JSONArray(), manufacturer = new JSONArray();
    if (record != null && record.getServiceUuids() != null) for (ParcelUuid value : record.getServiceUuids()) services.put(value.toString());
    if (record != null) {
        android.util.SparseArray<byte[]> data = record.getManufacturerSpecificData();
        // Match CoreBluetooth's manufacturer payload: company ID, little endian, then bytes.
        if (data.size() > 0) {
            int company = data.keyAt(0); manufacturer.put(company & 255).put((company >> 8) & 255);
            for (byte value : data.valueAt(0)) manufacturer.put(value & 255);
        }
    }
    String name = record == null ? null : record.getDeviceName();
    emit(object("event", "advertisement", "advertisement", object("device", id, "name", name == null ? JSONObject.NULL : name,
        "rssi", result.getRssi(), "services", services, "manufacturer_data", manufacturer,
        "connectable", Build.VERSION.SDK_INT >= 26 ? result.isConnectable() : JSONObject.NULL)));
}
public static void watch_native(long token) {
    main.post(() -> { try { prepare(); observers.add(token); emit(object("event", "state", "state", state())); }
        catch (Exception e) { watch_native_fail(token, e.toString()); } });
}
public static void unwatch_native(long token) { main.post(() -> observers.remove(token)); }
public static void cancel_native(long token) {
    main.post(() -> {
        Request request = null;
        for (Request value : active.values()) if (value.token == token) { request = value; break; }
        if (request != null) {
            String id = request.device;
            BluetoothGatt gatt = connections.remove(id); connected.remove(id);
            drain(id, "disconnected", "An active GATT operation was cancelled; reconnect before continuing");
            if (gatt != null) { gatt.disconnect(); gatt.close(); }
            emit(object("event", "disconnected", "device", id));
        } else {
            for (ArrayDeque<Request> queue : queues.values()) queue.removeIf(value -> value.token == token);
            queues.values().removeIf(ArrayDeque::isEmpty);
        }
    });
}
public static void request_native(String text, long token) {
    main.post(() -> {
        try {
            Request request = new Request(new JSONObject(text), token);
            switch (request.operation) {
                case "connect": case "services": case "read": case "write": case "notify":
                    ArrayDeque<Request> queue = queues.computeIfAbsent(request.device, key -> new ArrayDeque<>());
                    if (queue.size() >= 128) { fail(token, "busy", "Peripheral request queue is full"); return; }
                    queue.addLast(request); next(request.device); break;
                default: execute(request);
            }
        } catch (Exception e) { fail(token, "invalid_argument", e.toString()); }
    });
}
private static void execute(Request request) {
    try { command(request.body, request.token); }
    catch (SecurityException e) { fail(request.token, "permission_denied", e.toString()); }
    catch (Exception e) { fail(request.token, "platform", e.toString()); }
}
private static void command(JSONObject request, long token) throws Exception {
    prepare();
    String op = request.getString("op"), id = request.optString("device");
    if (op.equals("api_level")) { reply(token, Build.VERSION.SDK_INT); return; }
    if (op.equals("scan_needs_location")) { reply(token, scanNeedsLocation()); return; }
    if (!permitted()) { fail(token, "permission_denied", "Grant Bluetooth permissions before use"); return; }
    if (adapter == null) { fail(token, "unsupported", "No Bluetooth adapter"); return; }
    if (!adapter.isEnabled()) { fail(token, "powered_off", "Bluetooth is off"); return; }
    if (op.equals("scan")) {
        if (scanning) adapter.getBluetoothLeScanner().stopScan(scanner);
        List<ScanFilter> filters = new ArrayList<>();
        JSONArray values = request.getJSONArray("services");
        for (int i = 0; i < values.length(); i++) filters.add(new ScanFilter.Builder().setServiceUuid(new ParcelUuid(uuid(values.getString(i)))).build());
        duplicates = request.optBoolean("duplicates"); seen.clear(); scanning = true;
        adapter.getBluetoothLeScanner().startScan(filters, new ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build(), scanner);
        reply(token, null); return;
    }
    if (op.equals("stop_scan")) { adapter.getBluetoothLeScanner().stopScan(scanner); scanning = false; reply(token, null); return; }
    BluetoothGatt gatt = connections.get(id);
    if (op.equals("disconnect")) {
        connections.remove(id); connected.remove(id);
        drain(id, "disconnected", "Peripheral disconnected by app");
        if (gatt != null) { gatt.disconnect(); gatt.close(); emit(object("event", "disconnected", "device", id)); }
        reply(token, null); return;
    }
    if (!op.equals("connect") && (gatt == null || !connected.contains(id))) { fail(token, "disconnected", "Peripheral is disconnected"); return; }
    String pendingCharacteristic = request.optString("characteristic");
    main.postDelayed(() -> {
        Request running = active.get(id);
        if (running == null || running.token != token) return;
        connected.remove(id);
        drain(id, "timeout", "Bluetooth operation timed out; connection closed");
        BluetoothGatt expired = connections.remove(id);
        if (expired != null) { expired.disconnect(); expired.close(); }
        emit(object("event", "disconnected", "device", id));
    }, 20000);
    if (op.equals("connect")) {
        if (gatt != null && connected.contains(id)) { finish(id, null, 0); return; }
        BluetoothDevice device = devices.get(id);
        if (device == null) device = adapter.getRemoteDevice(id);
        BluetoothGatt connecting = device.connectGatt(DayRuntime.ctx.getApplicationContext(), false, callback, BluetoothDevice.TRANSPORT_LE);
        if (connecting == null) { fail(token, "platform", "Could not create GATT client"); }
        else connections.put(id, connecting);
        return;
    }
    if (op.equals("services")) {
        if (!gatt.discoverServices()) finish(id, null, BluetoothGatt.GATT_FAILURE);
        return;
    }
    BluetoothGattService service = gatt.getService(uuid(request.getString("service")));
    BluetoothGattCharacteristic characteristic = service == null ? null : service.getCharacteristic(uuid(pendingCharacteristic));
    if (characteristic == null) { fail(token, "not_found", "Discover the service and characteristic first"); return; }
    boolean accepted = false;
    if (op.equals("read")) accepted = gatt.readCharacteristic(characteristic);
    else if (op.equals("write")) {
        JSONArray array = request.getJSONArray("value"); byte[] value = new byte[array.length()];
        for (int i = 0; i < value.length; i++) value[i] = (byte)array.getInt(i);
        int type = request.optBoolean("with_response", true) ? BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT : BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE;
        if (Build.VERSION.SDK_INT >= 33) accepted = gatt.writeCharacteristic(characteristic, value, type) == BluetoothStatusCodes.SUCCESS;
        else { characteristic.setWriteType(type); characteristic.setValue(value); accepted = gatt.writeCharacteristic(characteristic); }
    } else if (op.equals("notify")) {
        boolean enabled = request.getBoolean("enabled");
        BluetoothGattDescriptor descriptor = characteristic.getDescriptor(uuid("2902"));
        if (descriptor != null && gatt.setCharacteristicNotification(characteristic, enabled)) {
            byte[] value = !enabled ? BluetoothGattDescriptor.DISABLE_NOTIFICATION_VALUE :
                (characteristic.getProperties() & BluetoothGattCharacteristic.PROPERTY_NOTIFY) != 0 ? BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE : BluetoothGattDescriptor.ENABLE_INDICATION_VALUE;
            if (Build.VERSION.SDK_INT >= 33) accepted = gatt.writeDescriptor(descriptor, value) == BluetoothStatusCodes.SUCCESS;
            else { descriptor.setValue(value); accepted = gatt.writeDescriptor(descriptor); }
        }
    }
    if (!accepted) finish(id, null, BluetoothGatt.GATT_FAILURE);
}
private static boolean matches(BluetoothGatt gatt, String op, BluetoothGattCharacteristic characteristic) {
    if (!current(gatt)) return false;
    Request request = active.get(gatt.getDevice().getAddress());
    return request != null && request.operation.equals(op) &&
        (characteristic == null || (uuid(request.characteristic).equals(characteristic.getUuid()) &&
         uuid(request.service).equals(characteristic.getService().getUuid())));
}
private static void value(BluetoothGatt gatt, BluetoothGattCharacteristic characteristic, byte[] bytes, int status, boolean read) {
    if (!current(gatt)) return;
    if (read && matches(gatt, "read", characteristic)) finish(gatt.getDevice().getAddress(), bytes(bytes), status);
    if (status == 0) emit(object("event", "value", "device", gatt.getDevice().getAddress(),
        "service", characteristic.getService().getUuid().toString(), "characteristic", characteristic.getUuid().toString(), "value", bytes(bytes)));
}
private static final BluetoothGattCallback callback = new BluetoothGattCallback() {
    public void onConnectionStateChange(BluetoothGatt gatt, int status, int state) {
        main.post(() -> {
            if (!current(gatt)) return;
            String id = gatt.getDevice().getAddress();
            if (state == BluetoothProfile.STATE_CONNECTED && status == 0) {
                connected.add(id);
                emit(object("event", "connected", "device", id));
                if (matches(gatt, "connect", null)) finish(id, null, 0);
            } else if (state == BluetoothProfile.STATE_DISCONNECTED || status != 0) {
                connections.remove(id, gatt); connected.remove(id); gatt.close();
                emit(object("event", "disconnected", "device", id));
                drain(id, "disconnected", "GATT disconnect status " + status);
            }
        });
    }
    public void onServicesDiscovered(BluetoothGatt gatt, int status) {
        main.post(() -> {
            if (!matches(gatt, "services", null)) return;
            JSONArray services = new JSONArray();
            for (BluetoothGattService service : gatt.getServices()) {
                JSONArray characteristics = new JSONArray();
                for (BluetoothGattCharacteristic c : service.getCharacteristics()) {
                    int p = c.getProperties();
                    characteristics.put(object("uuid", c.getUuid().toString(), "read", (p & 2) != 0, "write", (p & 8) != 0,
                        "write_without_response", (p & 4) != 0, "notify", (p & 16) != 0, "indicate", (p & 32) != 0));
                }
                services.put(object("uuid", service.getUuid().toString(), "characteristics", characteristics));
            }
            finish(gatt.getDevice().getAddress(), services, status);
        });
    }
    public void onCharacteristicRead(BluetoothGatt g, BluetoothGattCharacteristic c, int status) {
        if (Build.VERSION.SDK_INT < 33) { byte[] data = c.getValue(); main.post(() -> value(g,c,data,status,true)); }
    }
    public void onCharacteristicRead(BluetoothGatt g, BluetoothGattCharacteristic c, byte[] data, int status) { main.post(() -> value(g,c,data,status,true)); }
    public void onCharacteristicChanged(BluetoothGatt g, BluetoothGattCharacteristic c) {
        if (Build.VERSION.SDK_INT < 33) { byte[] data = c.getValue(); main.post(() -> value(g,c,data,0,false)); }
    }
    public void onCharacteristicChanged(BluetoothGatt g, BluetoothGattCharacteristic c, byte[] data) { main.post(() -> value(g,c,data,0,false)); }
    public void onCharacteristicWrite(BluetoothGatt g, BluetoothGattCharacteristic c, int status) { main.post(() -> { if (matches(g,"write",c)) finish(g.getDevice().getAddress(),null,status); }); }
    public void onDescriptorWrite(BluetoothGatt g, BluetoothGattDescriptor d, int status) { main.post(() -> { if (matches(g,"notify",d.getCharacteristic())) finish(g.getDevice().getAddress(),null,status); }); }
};
