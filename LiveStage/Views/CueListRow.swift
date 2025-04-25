import AVFoundation
//
//  CueListRow.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
import SwiftUI

struct CueListRow: View {
    @ObservedObject var cue: AnyCue

    var body: some View {
        HStack {
            if cue.cast(VideoCue.self) != nil{
                Image(systemName: "film")
            }
            
            Text(cue.title)
            Spacer()

            let totalSeconds = Int(cue.cueDuration)
            let minutes = totalSeconds / 60
            let seconds = totalSeconds % 60
            Text(String(format: "%d:%02d", minutes, seconds))
        }
        .padding()
        .background(cue.isSelected ? Color.accentColor : .clear)
        .cornerRadius(5)
        .onTapGesture {
            if cue.isSelected {
                openCueWindow(cue: cue)
            }
            cue.isSelected.toggle()
        }
    }

    func openCueWindow(cue: AnyCue) {
        let newWindow = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 500, height: 500),
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered,
            defer: false
        )
        newWindow.center()
        newWindow.setFrameAutosaveName("Edit Cue")
        newWindow.isReleasedWhenClosed = false
        newWindow.contentView = NSHostingView(
            rootView: EditCueView(cue: .constant(cue))
        )
        newWindow.makeKeyAndOrderFront(nil)
    }

}

/*
#Preview {
    CueListRow(cue: AnyCue(VideoCue(id: UUID(), title: "String", player: AVPlayer())))
}
*/
