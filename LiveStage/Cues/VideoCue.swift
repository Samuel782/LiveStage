import AVKit
//
//  VideoCue.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
import SwiftUI

class VideoCue: Cue {
    // Cue Protocol
    var id: UUID
    var title: String
    var notes: String?

    public let player: AVPlayer

    init(id: UUID, title: String, notes: String? = nil, player: AVPlayer) {
        self.id = id
        self.title = title
        self.notes = notes
        self.player = player
    }

    func cueView() -> AnyView {
        AnyView(
            Text("Preview del cue: \(title)")
                .padding()
        )
    }

}
