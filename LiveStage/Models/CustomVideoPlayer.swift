//
//  CustomVideoPlayer.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
import SwiftUI
import AVKit


struct CustomVideoPlayer: NSViewRepresentable {
    let player: AVPlayer

    func makeNSView(context: Context) -> AVPlayerView {
        let playerView = AVPlayerView()
        playerView.player = player

        // Disabilita i controlli
        playerView.controlsStyle = .none
        return playerView
    }

    func updateNSView(_ nsView: AVPlayerView, context: Context) {
        nsView.player = player
    }
}
