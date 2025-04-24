//
//  CueListRow.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
import SwiftUI
import AVFoundation

struct CueListRow: View {
    @ObservedObject var cue: AnyCue

    @State private var isSelected: Bool = false

    var body: some View {
        HStack {
            Text(cue.title)

            Text("[" + (cue.notes ?? " ") + "]")
            Spacer()
            
            
            
            Text("" + String(cue.cueDuration))

        }.padding()
            .background(isSelected ? Color.accentColor : .clear)
            .cornerRadius(5)
            .onTapGesture {
                if isSelected == true {
                    openCueWindow(cue: cue)
                }
                isSelected = true
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
