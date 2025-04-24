final class AnyCue: ObservableObject, Identifiable {
    let id: UUID
    var title: String {
        get { base.title }
        set { base.title = newValue }
    }
    var notes: String? {
        get { base.notes }
        set { base.notes = newValue }
    }

    private var base: any Cue

    init<T: Cue>(_ cue: T) {
        self.base = cue
        self.id = cue.id
    }

    func cueView() -> AnyView {
        base.cueView()
    }
}