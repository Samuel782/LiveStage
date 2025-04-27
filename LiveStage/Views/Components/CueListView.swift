//
//  CueListComponent.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 27/04/25.
//

import SwiftUI

struct CueListView: View {
    @Binding var cueList: [AnyCue]

    var body: some View {
        List {
            ForEach(cueList.indices, id: \.self) { index in
                CueListRow(cue: cueList[index])
                .onTapGesture {
                    cueList.indices.forEach { cueList[$0].isSelected = false }
                    cueList[index].isSelected.toggle()
                }
            }
            .onMove { source, destination in
                moveItem(from: source, to: destination)
            }
        }
        .padding()
        .frame(maxWidth: .infinity)
        .cornerRadius(10)

    }
    func moveItem(from source: IndexSet, to destination: Int) {
        cueList.move(fromOffsets: source, toOffset: destination)
    }
}
