import SwiftUI

extension AppModel {
    /// A model on the sample latch, with its status already loaded, for
    /// previews and snapshots.
    public static func sample(_ scenario: FakeLatchClient.Scenario) async -> AppModel {
        let model = AppModel(client: FakeLatchClient(scenario))
        await model.refreshStatus()
        return model
    }
}

/// Shows `content` with a sample model for `scenario` once it has loaded.
struct SamplePreview<Content: View>: View {
    var scenario: FakeLatchClient.Scenario
    @ViewBuilder var content: () -> Content
    @State private var model: AppModel?

    var body: some View {
        Group {
            if let model { content().environment(model) } else { ProgressView() }
        }
        .task { model = await .sample(scenario) }
    }
}

#Preview("Overview · serving") { SamplePreview(scenario: .serving) { MainView(screen: .overview) } }
#Preview("Overview · stopped") { SamplePreview(scenario: .stopped) { MainView(screen: .overview) } }
#Preview("Overview · logged out") { SamplePreview(scenario: .loggedOut) { MainView(screen: .overview) } }
#Preview("Overview · host error") { SamplePreview(scenario: .hostError) { MainView(screen: .overview) } }
#Preview("Overview · unreachable") { SamplePreview(scenario: .unreachable) { MainView(screen: .overview) } }
#Preview("Activity · serving") { SamplePreview(scenario: .serving) { MainView(screen: .activity) } }
#Preview("Activity · empty") { SamplePreview(scenario: .noActivity) { MainView(screen: .activity) } }
#Preview("Rules") { SamplePreview(scenario: .serving) { MainView(screen: .rules) } }
#Preview("Settings") { SamplePreview(scenario: .serving) { SettingsView() } }
#Preview("Menu") { SamplePreview(scenario: .serving) { VStack(alignment: .leading) { StatusMenu() }.padding() } }
