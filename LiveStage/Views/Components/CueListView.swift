//
//  CueListComponent.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 27/04/25.
//

import SwiftUI
struct CueListView: View {
    @Binding var cueList: [AnyCue]
    @State private var openedCue: AnyCue?

    var body: some View {
        List {
            ForEach(cueList) { cue in
                CueListRow(
                    cue: cue,
                    onDoubleTap: {
                        openedCue = cue
                    }
                )
                .contentShape(Rectangle())
                .onTapGesture {
                    select(cue: cue)
                }
            }
        }
        .sheet(item: $openedCue) { cue in
            EditCueView(cue: .constant(cue))
                .frame(width: 500, height: 500)
        }
    }

    private func select(cue: AnyCue) {
        for otherCue in cueList {
            otherCue.isSelected = (otherCue.id == cue.id)
            otherCue.updateBaseValues()
        }
    }
}
