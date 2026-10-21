import Foundation

/// What `latch rules show` prints for this Mac's own host.
public struct RulesShow: Codable, Equatable, Sendable {
    public var host: String
    public var everyAgent: Text?
    public var agents: [Agent]
    public var warning: String?

    public struct Text: Codable, Equatable, Sendable {
        public var text: String
        public var revision: String

        public init(text: String, revision: String) {
            self.text = text
            self.revision = revision
        }
    }

    public struct Agent: Codable, Equatable, Identifiable, Sendable {
        public var agentUid: String
        public var agentName: String?
        public var text: String?
        public var note: String?
        public var id: String { agentUid }

        public init(agentUid: String, agentName: String?, text: String?, note: String? = nil) {
            self.agentUid = agentUid
            self.agentName = agentName
            self.text = text
            self.note = note
        }
    }

    public init(host: String, everyAgent: Text?, agents: [Agent], warning: String? = nil) {
        self.host = host
        self.everyAgent = everyAgent
        self.agents = agents
        self.warning = warning
    }
}
