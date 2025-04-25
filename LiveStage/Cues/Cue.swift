//
//  Cue.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//

import Foundation
import SwiftUI

protocol Cue: Identifiable, ObservableObject {
    var id: UUID { get }
    var title: String { get set }
    var notes: String? { get set }
    var cueDuration: Double { get set }
    var isSelected: Bool { get set }
    
    
    @ViewBuilder func cueView() -> AnyView
}
