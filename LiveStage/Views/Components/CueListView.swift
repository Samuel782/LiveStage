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
                HStack {
                    if let index = cueList.firstIndex(where: { $0.id == cue.id }) {
                        Text("\(index + 1)")
                            .foregroundColor(.gray)
                            .frame(width: 24, height: 24)
                            .padding(0)
                    }
                    
                    Image(systemName: "arrow.up.and.down.circle.fill")
                        .foregroundColor(.gray)
                        .frame(width: 24, height: 24)

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
            .onMove { source, destination in
                moveItem(from: source, to: destination)
            }
        }
        .sheet(item: $openedCue) { cue in
            EditCueView(cue: .constant(cue))
                .frame(width: 500, height: 500)
        }
    }

    func moveItem(from source: IndexSet, to destination: Int) {
        cueList.move(fromOffsets: source, toOffset: destination)
    }

    private func select(cue: AnyCue) {
        for otherCue in cueList {
            otherCue.isSelected = (otherCue.id == cue.id)
            otherCue.updateBaseValues()
        }
    }
}
