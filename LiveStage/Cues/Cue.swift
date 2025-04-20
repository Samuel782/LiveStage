//
//  Cue.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//

import Foundation
import SwiftUI

class Cue: Identifiable, ObservableObject {
    var id = UUID()
    @Published var title: String
    @Published var notes: String?
    // var startTimecode: Timecode

    init(title: String, notes: String? = nil) {
        self.title = title
        self.notes = notes
    }

    func setTitle(title: String) {
        self.title = title
    }

    func cueView() -> some View {
        VStack {
        }
        .padding()
        .background(RoundedRectangle(cornerRadius: 10).strokeBorder())
    }

}
