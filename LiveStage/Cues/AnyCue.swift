//
//  AnyCue.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 24/04/25.
//

import Foundation
import SwiftUI

class AnyCue: ObservableObject, Cue {
    private let _id: () -> UUID
    private let _title: () -> String
    private let _setTitle: (String) -> Void
    private let _notes: () -> String?
    private let _setNotes: (String?) -> Void
    private let _cueView: () -> AnyView
    private let _cueDuration: () -> Double
    private let _setCueDuration: (Double) -> Void

    var id: UUID { _id() }
    var title: String {
        get { _title() }
        set { _setTitle(newValue) }
    }
    var notes: String? {
        get { _notes() }
        set { _setNotes(newValue) }
    }
    var cueDuration: Double{
        get { _cueDuration() }
        set { _setCueDuration(newValue)}
    }

    init<T: Cue>(_ base: T) {
        _id = { base.id }
        _title = { base.title }
        _setTitle = { base.title = $0 }
        _notes = { base.notes }
        _setNotes = { base.notes = $0 }
        _cueView = { base.cueView() }
        _cueDuration = { base.cueDuration }
        _setCueDuration = { base.cueDuration = $0}
    }

    func cueView() -> AnyView {
        _cueView()
    }
}
