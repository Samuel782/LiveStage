import AVFoundation
import SwiftUI

//
//  CueListRow.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//

struct CueListRow: View {
    @ObservedObject var cue: AnyCue
    @State private var openWindow: NSWindow? = nil

    var body: some View {
        HStack {
            if cue.cast(VideoCue.self) != nil {
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
        .onTapGesture(count: 2) {  //Double click to open window
            openCueWindow(cue: cue)
        }

    }

    func openCueWindow(cue: AnyCue) {
        if let existingWindow = openWindow {
            existingWindow.makeKeyAndOrderFront(nil)
        } else {
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
            self.openWindow = newWindow
        }
    }

}

/*
#Preview {
    CueListRow(cue: AnyCue(VideoCue(id: UUID(), title: "String", player: AVPlayer())))
}
*/
