import AVFoundation
import AVKit
import SwiftUI

class VideoCue: Cue, ObservableObject {

    var id: UUID
    var title: String
    var notes: String?

    @Published var cueDuration: Double
    var isSelected: Bool

    public let player: AVPlayer

    init(id: UUID, title: String, notes: String? = nil, player: AVPlayer) {
        self.id = id
        self.title = title
        self.notes = notes
        self.player = player
        self.isSelected = false
        self.cueDuration = 0.0

        guard let asset = player.currentItem?.asset else {
            return
        }
        loadDuration(from: asset)
    }

    private func loadDuration(from asset: AVAsset) {
        Task {
            do {
                let duration = try await asset.load(.duration)
                if duration.isValid {
                    let durationInSeconds = CMTimeGetSeconds(duration)
                    if durationInSeconds.isFinite {
                        DispatchQueue.main.async {
                            self.cueDuration = durationInSeconds
                            print("durata valida \(self.cueDuration)")
                        }
                    } else {
                        print("Durata non valida.")
                        DispatchQueue.main.async {
                            self.cueDuration = 0.0
                        }
                    }
                } else {
                    print("CMTime non valido.")
                    DispatchQueue.main.async {
                        self.cueDuration = 0.0
                    }
                }
            } catch {
                print("Errore nel caricamento della durata: \(error.localizedDescription)")
                DispatchQueue.main.async {
                    self.cueDuration = 0.0
                }
            }
        }
    }

    func cueView() -> AnyView {
        AnyView(
            VStack {
                Text("Preview del cue: \(title)")
                    .padding()

                Text("Durata: \(String(format: "%.2f", cueDuration)) s")
                    .padding()
            }
        )
    }
}
