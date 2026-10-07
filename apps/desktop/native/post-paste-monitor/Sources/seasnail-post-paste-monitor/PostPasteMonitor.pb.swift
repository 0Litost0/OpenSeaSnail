// Generated shape for proto/seasnail/native/v1/post_paste_monitor.proto.
// proto-sha256: 52be847f78b9348c3c7978538ea8c65fe6179914262fa8f3f1cb5418c531d040
// Keep in sync with the source proto via scripts/macos/build-post-paste-monitor.sh.
import Foundation
import SwiftProtobuf

struct Seasnail_Native_V1_MonitorRequest: Sendable {
    var schemaVersion: UInt32 = 0
    var targetPid: Int32 = 0
    var pastedText: String = ""
    var timeoutMs: UInt32 = 0
    var unknownFields = SwiftProtobuf.UnknownStorage()
}

struct Seasnail_Native_V1_MonitorEvent: Sendable {
    enum OneOf_Payload: Equatable, Sendable {
        case ready(Ready)
        case changed(Changed)
        case finished(Finished)
    }
    struct Ready: Equatable, Sendable {
        var initialValue: String = ""
        var unknownFields = SwiftProtobuf.UnknownStorage()
    }
    struct Changed: Equatable, Sendable {
        var currentValue: String = ""
        var unknownFields = SwiftProtobuf.UnknownStorage()
    }
    struct Finished: Equatable, Sendable {
        enum Reason: SwiftProtobuf.Enum, Sendable {
            typealias RawValue = Int
            case reasonUnspecified
            case timeout
            case noElement
            case noValue
            case focusLost
            case cancelled
            case internalError
            case UNRECOGNIZED(Int)
            init() { self = .reasonUnspecified }
            init?(rawValue: Int) {
                switch rawValue {
                case 0: self = .reasonUnspecified
                case 1: self = .timeout
                case 2: self = .noElement
                case 3: self = .noValue
                case 4: self = .focusLost
                case 5: self = .cancelled
                case 6: self = .internalError
                default: self = .UNRECOGNIZED(rawValue)
                }
            }
            var rawValue: Int {
                switch self {
                case .reasonUnspecified: 0
                case .timeout: 1
                case .noElement: 2
                case .noValue: 3
                case .focusLost: 4
                case .cancelled: 5
                case .internalError: 6
                case .UNRECOGNIZED(let value): value
                }
            }
        }
        var reason: Reason = .reasonUnspecified
        var unknownFields = SwiftProtobuf.UnknownStorage()
    }
    var schemaVersion: UInt32 = 0
    var payload: OneOf_Payload?
    var unknownFields = SwiftProtobuf.UnknownStorage()
}

extension Seasnail_Native_V1_MonitorRequest: SwiftProtobuf.Message, SwiftProtobuf._MessageImplementationBase, SwiftProtobuf._ProtoNameProviding {
    static let protoMessageName = "seasnail.native.v1.MonitorRequest"
    static let _protobuf_nameMap = SwiftProtobuf._NameMap(bytecode: "\0\u{3}schema_version\0\u{3}target_pid\0\u{3}pasted_text\0\u{3}timeout_ms\0")
    mutating func decodeMessage<D: SwiftProtobuf.Decoder>(decoder: inout D) throws {
        while let fieldNumber = try decoder.nextFieldNumber() {
            switch fieldNumber {
            case 1: try decoder.decodeSingularUInt32Field(value: &schemaVersion)
            case 2: try decoder.decodeSingularInt32Field(value: &targetPid)
            case 3: try decoder.decodeSingularStringField(value: &pastedText)
            case 4: try decoder.decodeSingularUInt32Field(value: &timeoutMs)
            default: break
            }
        }
    }
    func traverse<V: SwiftProtobuf.Visitor>(visitor: inout V) throws {
        if schemaVersion != 0 { try visitor.visitSingularUInt32Field(value: schemaVersion, fieldNumber: 1) }
        if targetPid != 0 { try visitor.visitSingularInt32Field(value: targetPid, fieldNumber: 2) }
        if !pastedText.isEmpty { try visitor.visitSingularStringField(value: pastedText, fieldNumber: 3) }
        if timeoutMs != 0 { try visitor.visitSingularUInt32Field(value: timeoutMs, fieldNumber: 4) }
        try unknownFields.traverse(visitor: &visitor)
    }
}

extension Seasnail_Native_V1_MonitorEvent.Ready: SwiftProtobuf.Message, SwiftProtobuf._MessageImplementationBase, SwiftProtobuf._ProtoNameProviding {
    static let protoMessageName = "seasnail.native.v1.MonitorEvent.Ready"
    static let _protobuf_nameMap = SwiftProtobuf._NameMap(bytecode: "\0\u{3}initial_value\0")
    mutating func decodeMessage<D: SwiftProtobuf.Decoder>(decoder: inout D) throws {
        while let fieldNumber = try decoder.nextFieldNumber() {
            if fieldNumber == 1 { try decoder.decodeSingularStringField(value: &initialValue) }
        }
    }
    func traverse<V: SwiftProtobuf.Visitor>(visitor: inout V) throws {
        if !initialValue.isEmpty { try visitor.visitSingularStringField(value: initialValue, fieldNumber: 1) }
        try unknownFields.traverse(visitor: &visitor)
    }
}

extension Seasnail_Native_V1_MonitorEvent.Changed: SwiftProtobuf.Message, SwiftProtobuf._MessageImplementationBase, SwiftProtobuf._ProtoNameProviding {
    static let protoMessageName = "seasnail.native.v1.MonitorEvent.Changed"
    static let _protobuf_nameMap = SwiftProtobuf._NameMap(bytecode: "\0\u{3}current_value\0")
    mutating func decodeMessage<D: SwiftProtobuf.Decoder>(decoder: inout D) throws {
        while let fieldNumber = try decoder.nextFieldNumber() {
            if fieldNumber == 1 { try decoder.decodeSingularStringField(value: &currentValue) }
        }
    }
    func traverse<V: SwiftProtobuf.Visitor>(visitor: inout V) throws {
        if !currentValue.isEmpty { try visitor.visitSingularStringField(value: currentValue, fieldNumber: 1) }
        try unknownFields.traverse(visitor: &visitor)
    }
}

extension Seasnail_Native_V1_MonitorEvent.Finished: SwiftProtobuf.Message, SwiftProtobuf._MessageImplementationBase, SwiftProtobuf._ProtoNameProviding {
    static let protoMessageName = "seasnail.native.v1.MonitorEvent.Finished"
    static let _protobuf_nameMap = SwiftProtobuf._NameMap(bytecode: "\0\u{1}reason\0")
    mutating func decodeMessage<D: SwiftProtobuf.Decoder>(decoder: inout D) throws {
        while let fieldNumber = try decoder.nextFieldNumber() {
            if fieldNumber == 1 { try decoder.decodeSingularEnumField(value: &reason) }
        }
    }
    func traverse<V: SwiftProtobuf.Visitor>(visitor: inout V) throws {
        if reason != .reasonUnspecified { try visitor.visitSingularEnumField(value: reason, fieldNumber: 1) }
        try unknownFields.traverse(visitor: &visitor)
    }
}

extension Seasnail_Native_V1_MonitorEvent: SwiftProtobuf.Message, SwiftProtobuf._MessageImplementationBase, SwiftProtobuf._ProtoNameProviding {
    static let protoMessageName = "seasnail.native.v1.MonitorEvent"
    static let _protobuf_nameMap = SwiftProtobuf._NameMap(bytecode: "\0\u{1}schema_version\0\u{1}ready\0\u{1}changed\0\u{1}finished\0")
    mutating func decodeMessage<D: SwiftProtobuf.Decoder>(decoder: inout D) throws {
        while let fieldNumber = try decoder.nextFieldNumber() {
            switch fieldNumber {
            case 1: try decoder.decodeSingularUInt32Field(value: &schemaVersion)
            case 2:
                var value: Ready?
                try decoder.decodeSingularMessageField(value: &value)
                if let value { payload = .ready(value) }
            case 3:
                var value: Changed?
                try decoder.decodeSingularMessageField(value: &value)
                if let value { payload = .changed(value) }
            case 4:
                var value: Finished?
                try decoder.decodeSingularMessageField(value: &value)
                if let value { payload = .finished(value) }
            default: break
            }
        }
    }
    func traverse<V: SwiftProtobuf.Visitor>(visitor: inout V) throws {
        if schemaVersion != 0 { try visitor.visitSingularUInt32Field(value: schemaVersion, fieldNumber: 1) }
        switch payload {
        case .ready(let value): try visitor.visitSingularMessageField(value: value, fieldNumber: 2)
        case .changed(let value): try visitor.visitSingularMessageField(value: value, fieldNumber: 3)
        case .finished(let value): try visitor.visitSingularMessageField(value: value, fieldNumber: 4)
        case nil: break
        }
        try unknownFields.traverse(visitor: &visitor)
    }
}
