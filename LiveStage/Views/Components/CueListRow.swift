import SwiftUI

//
//  CueListRow.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
struct CueListRow: View {
    @ObservedObject var cue: AnyCue
    var onDoubleTap: () -> Void

    var body: some View {
        HStack {
            if cue.cast(VideoCue.self) != nil {
                Image(systemName: "film")
                    .allowsHitTesting(false)
            }

            Text(cue.title)
                .allowsHitTesting(false)
            Spacer()

            let totalSeconds = Int(cue.cueDuration)
            let minutes = totalSeconds / 60
            let seconds = totalSeconds % 60
            Text(String(format: "%d:%02d", minutes, seconds))
                .allowsHitTesting(false)
        }
        .padding()
        .background(cue.isSelected ? Color.accentColor.opacity(0.5) : .clear)
        .cornerRadius(5)
        .onTapGesture(count: 2, perform: onDoubleTap)
    }
}
