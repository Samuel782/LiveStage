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
class AnyCue: ObservableObject, Identifiable {
    
    // The base cue object
    private let base: any Cue
    
    // Published properties for dynamic changes
    @Published var title: String
    @Published var isSelected: Bool
    @Published var cueDuration: Double
    
    var notes: String?
    var id: UUID { base.id }
    
    init<T: Cue>(_ base: T) {
        self.base = base
        self.title = base.title
        self.isSelected = base.isSelected
        self.notes = base.notes
        self.cueDuration = base.cueDuration
        
    }

    // Function to get the view associated with this cue
    func cueView() -> AnyView {
        base.cueView()
    }
    
    /// Attempts to cast the underlying object to a specific type.
    func cast<T>(_ type: T.Type) -> T? {
        return base as? T
    }
    
    // Ensure the base object is updated when changes are made to the AnyCue properties
    func updateBaseValues() {
        base.title = title
        base.isSelected = isSelected
        base.notes = notes
        base.cueDuration = cueDuration
    }
}
