//
//  AnyCue.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 24/04/25.
//
import Foundation
import SwiftUI

/// Type-erased wrapper for any type conforming to the `Cue` protocol.
/// Allows storing heterogeneous cue types in a single collection.
class AnyCue: ObservableObject, Cue {
    
    /// Holds the original concrete instance to allow casting later.
    private let base: Any

    /// Stored closures to access and modify the wrapped `Cue` properties.
    private let _id: () -> UUID
    
    @Published private var _title: String
    private let _setTitle: (String) -> Void
    
    private let _notes: () -> String?
    private let _setNotes: (String?) -> Void
    private let _cueView: () -> AnyView
    private let _cueDuration: () -> Double
    private let _setCueDuration: (Double) -> Void

    @Published private var _isSelected: Bool
    private let _setIsSelected: (Bool) -> Void

    var id: UUID { _id() }
    var title: String {
        get { _title }
        set {
            _title = newValue
            _setTitle(newValue)
        }
    }
    var notes: String? {
        get { _notes() }
        set { _setNotes(newValue) }
    }
    var cueDuration: Double {
        get { _cueDuration() }
        set { _setCueDuration(newValue) }
    }
    var isSelected: Bool {
        get { _isSelected }
        set {
            _isSelected = newValue
            _setIsSelected(newValue)
        }
    }

    init<T: Cue>(_ base: T) {
        self.base = base
        self._title =  base.title
        self._isSelected = base.isSelected
        _id = { base.id }
        _setTitle = { base.title = $0 }
        _notes = { base.notes }
        _setNotes = { base.notes = $0 }
        _cueView = { base.cueView() }
        _cueDuration = { base.cueDuration }
        _setCueDuration = { base.cueDuration = $0 }
        _setIsSelected = { base.isSelected = $0 }
    }

    func cueView() -> AnyView {
        _cueView()
    }
    
    /// Attempts to cast the underlying object to a specific type.
    ///
    /// - Parameter type: The type to cast to (e.g., `VideoCue.self`).
    /// - Returns: The casted object if successful, or `nil` otherwise.
    func cast<T>(_ type: T.Type) -> T? {
        return base as? T
    }
}
