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
            Text(cue.title)

            Text("[" + (cue.notes ?? " ")  + "]")
            Spacer()

            // Timecode  here
            Text("00:00:00:00")

        }
        .onTapGesture {
            openCueWindow(cue: cue)
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
        newWindow.contentView = NSHostingView(rootView: EditCueView(cue: .constant(cue)))
        newWindow.makeKeyAndOrderFront(nil)
    }

}
