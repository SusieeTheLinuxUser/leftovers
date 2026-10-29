import QtQuick
import QtQuick.Layouts
import org.kde.plasma.plasmoid
import org.kde.plasma.core as PlasmaCore
import org.kde.plasma.components as PlasmaComponents
import org.kde.plasma.plasma5support as P5Support
import org.kde.kirigami as Kirigami

PlasmoidItem {
    id: root
    property var providers: []
    property real now: Date.now()
    readonly property string script: Qt.resolvedUrl("../code/limits.py").toString().replace("file://", "")
    readonly property var tints: ({ Claude: "#d97757", Codex: "#ffffff", Cursor: "#a78bfa", Kimi: "#3b82f6" })
    // QML hands JSON arrays back as list types without flatMap, hence the plain loop.
    readonly property var all: {
        const out = []
        for (const p of providers)
            for (const l of p.limits)
                out.push(Object.assign({ provider: p.name, account: p.account }, l))
        return out
    }
    // The panel shows whichever limit is closest to running out.
    readonly property var lowest: all.reduce((a, b) => (!a || b.used > a.used ? b : a), null)
    readonly property real lowestLeft: lowest ? Math.max(0, 100 - lowest.used) : 0

    function tint(name) { return tints[name] || "#a3a3a3" }

    Plasmoid.backgroundHints: PlasmaCore.Types.NoBackground  // on the desktop the black card is the background
    toolTipMainText: "Leftovers"
    toolTipSubText: all.map(l => `${l.provider}${l.account ? ` (${l.account})` : ""} ${l.name}: ${Math.round(100 - l.used)}% left`).join("\n")

    P5Support.DataSource {
        id: exec
        engine: "executable"
        onNewData: (source, data) => {
            disconnectSource(source)
            try {
                root.providers = JSON.parse(data.stdout).providers
            } catch (e) {
                console.warn("leftovers:", data.stderr)
            }
        }
    }

    // Claude's usage endpoint returns 429 when polled much faster than this.
    Timer {
        interval: 5 * 60 * 1000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: exec.connectSource(`python3 '${root.script}'`)
    }
    Timer {
        interval: 30 * 1000
        running: true
        repeat: true
        onTriggered: root.now = Date.now()
    }

    compactRepresentation: MouseArea {
        Layout.minimumWidth: compactLabel.implicitWidth + Kirigami.Units.smallSpacing * 2
        onClicked: root.expanded = !root.expanded
        PlasmaComponents.Label {
            id: compactLabel
            anchors.centerIn: parent
            text: root.lowest ? `${Math.round(root.lowestLeft)}%` : "…"
            color: root.lowest ? root.tint(root.lowest.provider) : Kirigami.Theme.textColor
            font.weight: Font.DemiBold
        }
        Rectangle {
            visible: !!root.lowest
            anchors { top: compactLabel.bottom; left: compactLabel.left; right: compactLabel.right }
            height: 2
            radius: 1
            color: Qt.rgba(1, 1, 1, 0.12)
            Rectangle {
                width: parent.width * root.lowestLeft / 100
                height: parent.height
                radius: parent.radius
                color: compactLabel.color
            }
        }
    }

    fullRepresentation: Rectangle {
        Layout.preferredWidth: Kirigami.Units.gridUnit * 20
        Layout.preferredHeight: content.implicitHeight + content.anchors.margins * 2
        color: "#000000"
        radius: 10

        ColumnLayout {
            id: content
            anchors { fill: parent; margins: Kirigami.Units.gridUnit }
            spacing: Kirigami.Units.largeSpacing

            Repeater {
                model: root.providers
                delegate: ColumnLayout {
                    id: provider
                    required property var modelData
                    required property int index
                    readonly property color tint: root.tint(modelData.name)
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing * 1.5

                    Rectangle {
                        visible: provider.index > 0
                        Layout.fillWidth: true
                        Layout.bottomMargin: Kirigami.Units.smallSpacing
                        implicitHeight: 1
                        color: "#141414"
                    }
                    RowLayout {
                        spacing: Kirigami.Units.smallSpacing * 1.5
                        Rectangle {
                            implicitWidth: 6
                            implicitHeight: 6
                            radius: 3
                            color: provider.tint
                        }
                        Text {
                            text: provider.modelData.name
                            color: provider.tint
                            font.weight: Font.DemiBold
                        }
                        Text {
                            text: provider.modelData.account
                            color: "#5c5c5c"
                            font: Kirigami.Theme.smallFont
                            elide: Text.ElideMiddle
                            horizontalAlignment: Text.AlignRight
                            Layout.fillWidth: true
                        }
                    }
                    Text {
                        visible: !!provider.modelData.error
                        text: (provider.modelData.limits.length ? "last known numbers · " : "") + provider.modelData.error
                        color: "#5c5c5c"
                        font: Kirigami.Theme.smallFont
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }
                    Repeater {
                        model: provider.modelData.limits
                        delegate: Bar {
                            required property var modelData
                            Layout.fillWidth: true
                            label: modelData.name
                            used: modelData.used
                            resets: modelData.resets
                            tint: provider.tint
                            now: root.now
                        }
                    }
                }
            }
            Text {
                visible: root.providers.length === 0
                text: "Log in to claude, codex, cursor-agent or kimi in a terminal and they'll show up here."
                color: "#a3a3a3"
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
        }
    }
}
