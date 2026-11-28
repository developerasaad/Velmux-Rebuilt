package com.velmux.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun OptionSheet(
    title: String,
    choices: List<String>,
    selectedIndex: Int,
    onSelect: (Int) -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = MaterialTheme.colorScheme.surface,
    ) {
        Column(Modifier.fillMaxWidth().heightIn(max = 600.dp)) {
            Text(
                title,
                Modifier.padding(horizontal = 24.dp, vertical = 12.dp).semantics { heading() },
                style = MaterialTheme.typography.titleLarge,
            )
            LazyColumn(
                modifier = Modifier.weight(1f, fill = false).selectableGroup(),
                contentPadding = PaddingValues(bottom = 20.dp),
            ) {
                itemsIndexed(choices) { index, label ->
                    Row(
                        Modifier.fillMaxWidth().heightIn(min = 56.dp).selectable(
                            selected = index == selectedIndex,
                            role = Role.RadioButton,
                            onClick = {
                                onDismiss()
                                onSelect(index)
                            },
                        ).padding(horizontal = 20.dp, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(selected = index == selectedIndex, onClick = null)
                        Text(label, Modifier.padding(start = 16.dp).weight(1f), style = MaterialTheme.typography.bodyLarge)
                    }
                }
            }
        }
    }
}
