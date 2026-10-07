import AppKit
import Foundation
import SwiftProtobuf

private let maxFrameBytes = 1024 * 1024
private nonisolated(unsafe) var applicationElement: AXUIElement?
private nonisolated(unsafe) var monitoredElement: AXUIElement?
private nonisolated(unsafe) var observer: AXObserver?
private nonisolated(unsafe) var lastValue: String?

private func diagnostic(_ code: String) {
    FileHandle.standardError.write(Data((code + "\n").utf8))
}

private func readExactly(_ count: Int) -> Data? {
    var data = Data()
    while data.count < count {
        let chunk = FileHandle.standardInput.readData(ofLength: count - data.count)
        if chunk.isEmpty { return nil }
        data.append(chunk)
    }
    return data
}

@discardableResult
private func writeFrame(_ event: Seasnail_Native_V1_MonitorEvent) -> Bool {
    guard let body = try? event.serializedData(), body.count <= maxFrameBytes else {
        diagnostic("E_ENCODE")
        return false
    }
    var length = UInt32(body.count).bigEndian
    var frame = Data(bytes: &length, count: 4)
    frame.append(body)
    FileHandle.standardOutput.write(frame)
    return true
}

private func finish(_ reason: Seasnail_Native_V1_MonitorEvent.Finished.Reason) {
    var event = Seasnail_Native_V1_MonitorEvent()
    event.schemaVersion = 1
    var finished = Seasnail_Native_V1_MonitorEvent.Finished()
    finished.reason = reason
    event.payload = .finished(finished)
    writeFrame(event)
    CFRunLoopStop(CFRunLoopGetMain())
}

private func currentValue() -> String? {
    guard let element = monitoredElement else { return nil }
    var value: AnyObject?
    guard AXUIElementCopyAttributeValue(element, kAXValueAttribute as CFString, &value) == .success
    else { return nil }
    return value as? String
}

private let callback: AXObserverCallback = { _, _, _, _ in
    guard let value = currentValue() else {
        finish(.noValue)
        return
    }
    guard value != lastValue else { return }
    var event = Seasnail_Native_V1_MonitorEvent()
    event.schemaVersion = 1
    var changed = Seasnail_Native_V1_MonitorEvent.Changed()
    changed.currentValue = value
    event.payload = .changed(changed)
    guard writeFrame(event) else {
        finish(.internalError)
        return
    }
    lastValue = value
}

if CommandLine.arguments.dropFirst().first == "--self-test" {
    var request = Seasnail_Native_V1_MonitorRequest()
    request.schemaVersion = 1
    request.targetPid = 42
    request.pastedText = "line one\n样式"
    request.timeoutMs = 30_000
    guard let encoded = try? request.serializedData(),
          let decoded = try? Seasnail_Native_V1_MonitorRequest(serializedBytes: encoded),
          decoded.schemaVersion == 1, decoded.pastedText == request.pastedText
    else { exit(1) }
    var event = Seasnail_Native_V1_MonitorEvent()
    event.schemaVersion = 1
    var changed = Seasnail_Native_V1_MonitorEvent.Changed()
    changed.currentValue = request.pastedText
    event.payload = .changed(changed)
    guard let eventBytes = try? event.serializedData(), !eventBytes.isEmpty else { exit(1) }
    exit(0)
}

guard let header = readExactly(4) else { diagnostic("E_HEADER"); exit(2) }
let payloadLength = header.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
guard payloadLength > 0, payloadLength <= maxFrameBytes,
      let payload = readExactly(Int(payloadLength)),
      let request = try? Seasnail_Native_V1_MonitorRequest(serializedBytes: payload),
      request.schemaVersion == 1, request.targetPid > 0,
      !request.pastedText.isEmpty, request.timeoutMs > 0, request.timeoutMs <= 30_000
else { diagnostic("E_REQUEST"); exit(2) }

let appElement = AXUIElementCreateApplication(pid_t(request.targetPid))
applicationElement = appElement
var focused: AnyObject?
var focusResult: AXError = .failure
for attempt in 0..<5 {
    focusResult = AXUIElementCopyAttributeValue(
        appElement, kAXFocusedUIElementAttribute as CFString, &focused
    )
    if focusResult == .success, focused != nil { break }
    if attempt < 4 { Thread.sleep(forTimeInterval: 0.3) }
}
guard focusResult == .success, let focused else {
    finish(.noElement)
    diagnostic("E_NO_ELEMENT")
    exit(0)
}
monitoredElement = (focused as! AXUIElement)
guard let initial = currentValue() else {
    finish(.noValue)
    diagnostic("E_NO_VALUE")
    exit(0)
}
lastValue = initial
var readyEvent = Seasnail_Native_V1_MonitorEvent()
readyEvent.schemaVersion = 1
var ready = Seasnail_Native_V1_MonitorEvent.Ready()
ready.initialValue = initial
readyEvent.payload = .ready(ready)
writeFrame(readyEvent)

var createdObserver: AXObserver?
guard AXObserverCreate(pid_t(request.targetPid), callback, &createdObserver) == .success,
      let createdObserver else {
    finish(.internalError)
    diagnostic("E_OBSERVER")
    exit(1)
}
observer = createdObserver
guard AXObserverAddNotification(
    createdObserver, monitoredElement!, kAXValueChangedNotification as CFString, nil
) == .success else {
    finish(.internalError)
    diagnostic("E_NOTIFICATION")
    exit(1)
}
CFRunLoopAddSource(
    CFRunLoopGetMain(), AXObserverGetRunLoopSource(createdObserver), .commonModes
)

let focusTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { _ in
    var current: AnyObject?
    guard let applicationElement, AXUIElementCopyAttributeValue(
        applicationElement, kAXFocusedUIElementAttribute as CFString, &current
    ) == .success, let current, let monitoredElement,
          CFEqual(current, monitoredElement)
    else {
        finish(.focusLost)
        return
    }
}
RunLoop.main.add(focusTimer, forMode: .common)
DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(Int(request.timeoutMs))) {
    finish(.timeout)
}
signal(SIGTERM, SIG_IGN)
let signalSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
signalSource.setEventHandler { finish(.cancelled) }
signalSource.resume()
CFRunLoopRun()
