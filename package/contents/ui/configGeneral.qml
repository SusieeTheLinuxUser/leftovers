import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami
import org.kde.kcmutils as KCM

KCM.SimpleKCM {
    id: root

    property var cfg_hiddenProviders: []
    property var cfg_hiddenProvidersDefault: []

    Kirigami.FormLayout {
        Repeater {
            model: ["Claude", "Codex", "Grok", "Antigravity", "OpenRouter"]
            delegate: QQC2.CheckBox {
                required property string modelData
                required property int index

                text: modelData
                Kirigami.FormData.label: index === 0 ? "Show:" : ""
                checked: !root.cfg_hiddenProviders.includes(modelData)
                // New array, never an in-place change: the dialog's Apply button watches for it.
                onToggled: root.cfg_hiddenProviders = checked
                    ? root.cfg_hiddenProviders.filter(p => p !== modelData)
                    : root.cfg_hiddenProviders.concat(modelData)
            }
        }
    }
}