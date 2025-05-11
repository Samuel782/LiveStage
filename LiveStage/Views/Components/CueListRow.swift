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
                    .allowsHitTesting(false)  // Removes annoying delay when tapping on text/images
            }

            Text(cue.title)
                .allowsHitTesting(false)
            Spacer()

            Text(formattedDuration(cue.cueDuration))
                .allowsHitTesting(false)
         
        }
        .padding()
        .background(cue.isSelected ? Color.accentColor.opacity(0.5) : .clear)
        .cornerRadius(5)
        .onTapGesture(count: 2, perform: onDoubleTap)
    }
    
    
    func formattedDuration(_ duration: Double) -> String {
        guard duration > 0 else { return "ND" }
        let totalSeconds = Int(duration)
        let minutes = totalSeconds / 60
        let seconds = totalSeconds % 60
        return String(format: "%d:%02d", minutes, seconds)
    }
}
