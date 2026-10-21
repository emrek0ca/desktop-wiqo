(function(){
  var post=function(){};
  try{var h=window.webkit.messageHandlers.wiqoApp;post=function(v){try{h.postMessage(v)}catch(e){}}}catch(e){}
  var state={};var saved=window.__WIQO_STATE__||{};for(var k0 in saved){if(Object.prototype.hasOwnProperty.call(saved,k0))state[k0]=String(saved[k0])}
  var freeze=function(o){if(o&&typeof o==='object'&&!Object.isFrozen(o)){Object.freeze(o);Object.keys(o).forEach(function(k){freeze(o[k])})}return o};
  var data=window.__WIQO_DATA__===undefined?null:freeze(window.__WIQO_DATA__);
  var text=function(t,n){return String(t===undefined||t===null?'':t).slice(0,n)};
  /* wiqo.getJSON(url, path): a public https JSON/CSV address read by the server (the page has no network), resolved with the part named by path */
  var jobs={},seq=0;
  window.__wiqoJSON=function(id,ok,value){var j=jobs[id];if(!j)return;delete jobs[id];if(ok)j.r(value);else j.j(new Error(String(value||'Hata')))};
  var api={
    data:data,
    load:function(k){k=text(k,40);return Object.prototype.hasOwnProperty.call(state,k)?state[k]:null},
    save:function(k,v){k=text(k,40);v=text(v,2000);if(!k)return;state[k]=v;post({save:{k:k,v:v}})},
    copy:function(t){post({copy:text(t,20000)})},
    share:function(t){post({share:text(t,20000)})},
    haptic:function(kind){post({haptic:kind==='success'||kind==='warning'||kind==='error'?kind:'light'})},
    ask:function(t){post({ask:text(t,500)})},
    getJSON:function(url,path){return new Promise(function(r,j){var id=++seq;jobs[id]={r:r,j:j};post({json:{id:id,url:text(url,1500),path:text(path,200)}});setTimeout(function(){if(jobs[id]){delete jobs[id];j(new Error('Zaman aşımı'))}},25000)})}
  };
  try{Object.defineProperty(window,'wiqo',{value:Object.freeze(api)})}catch(e){}
  window.WIQO_DATA=data;
  var problem=function(m){post({err:text(m,200)})};
  window.addEventListener('error',function(e){problem(e&&e.message?e.message:'Betik hatası')});
  window.addEventListener('unhandledrejection',function(e){problem(e&&e.reason&&e.reason.message?e.reason.message:'İşlenmeyen hata')});
  window.addEventListener('load',function(){setTimeout(function(){
    var b=document.body;if(!b)return;
    var drawn=b.innerText.trim().length>2||b.querySelector('canvas,svg,img,button,input,select,textarea,table');
    var el=document.documentElement;
    post({check:{blank:!drawn,overflow:el.scrollWidth>el.clientWidth+4}});
  },800)});
})();
