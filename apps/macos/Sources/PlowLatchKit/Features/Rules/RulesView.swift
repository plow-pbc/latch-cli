import SwiftUI

/// The host's standing rules for agents, read-only.
struct RulesView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.theme) private var theme

    var body: some View {
        content
            .navigationTitle("Rules")
            .task { await model.loadRules() }
    }

    @ViewBuilder private var content: some View {
        switch model.rules {
        case .loading:
            ProgressView()
        case let .failed(.failed(failure), nil) where failure.code == "not_logged_in":
            Unavailable(kind: .notLoggedIn) { reload() }
        case let .failed(error, nil):
            Unavailable(kind: .unreachable(error.message)) { reload() }
        default:
            if let rules = model.rules.value { page(rules) }
        }
    }

    private func page(_ rules: RulesShow) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: theme.space.large) {
                block("Every agent", text: rules.everyAgent?.text)
                ForEach(rules.agents) { agent in
                    block(agent.agentName ?? agent.agentUid, text: agent.text, note: agent.note)
                }
                if let warning = rules.warning {
                    Text(warning)
                        .font(theme.type.detail)
                        .foregroundStyle(theme.colors.secondaryText)
                }
            }
            .padding(theme.space.page)
            .frame(maxWidth: 720, alignment: .leading)
        }
    }

    private func block(_ title: String, text: String?, note: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: theme.space.small) {
            Text(title).font(theme.type.heading)
            Text(text ?? "No rules.")
                .foregroundStyle(text == nil ? theme.colors.secondaryText : .primary)
                .textSelection(.enabled)
            if let note {
                Text(note).font(theme.type.detail).foregroundStyle(theme.colors.secondaryText)
            }
        }
        .padding(theme.space.large)
        .frame(maxWidth: .infinity, alignment: .leading)
        .latchSurface(.card)
    }

    private func reload() {
        Task { await model.loadRules() }
    }
}
